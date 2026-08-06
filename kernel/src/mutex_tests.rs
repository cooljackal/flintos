// SPDX-License-Identifier: Apache-2.0

//! Priority-inversion tests for `crate::mutex`.
//!
//! Inversion is the reason this module exists, so these drive the scenarios
//! directly rather than exercising the API surface.
//!
//! One caveat worth stating plainly. `lock()` on a held mutex queues the caller
//! and returns `Blocked`; the *caller* then asks for a switch. On a host
//! nothing suspends, so these tests drive the scheduler by hand and `run(id)`
//! stands in for the switch. That covers the bookkeeping — who owns what, who
//! is boosted, who is queued — which is where every bug in this module has
//! been. It is not a test of preemption. `make test-target` covers that on
//! silicon, where a real timer really does interrupt.

use super::*;
use crate::scheduler::TaskState;
use crate::testsupport as ts;

// Stand-ins for user-side Mutex objects. Any non-zero usize works: the table
// keys on the value and never dereferences it.
const M1: usize = 0x1000;
const M2: usize = 0x2000;
const M3: usize = 0x3000;

/// Current owner of `addr`, or `None` if the slot is free.
fn owner_of(addr: usize) -> Option<u32> {
    table()
        .iter()
        .find(|e| e.addr == addr && e.owner != NO_TASK)
        .map(|e| e.owner)
}

#[test]
fn owner_inherits_the_blocked_waiter_priority() {
    let _k = ts::lock();
    let low = ts::task(40);
    let high = ts::task(5);

    ts::run(low);
    assert!(lock(M1), "an uncontended lock is granted");

    ts::run(high);
    assert!(lock(M1), "a blocking lock reports ownership on resume");

    assert_eq!(
        ts::prio_of(low),
        5,
        "the holder must be lifted to the waiter's priority, or anything \
         between 5 and 40 preempts it while high waits"
    );
    assert_eq!(ts::base_of(low), 40, "the base priority is not overwritten");
    assert_eq!(ts::state_of(high), TaskState::BlockedMutex);
}

#[test]
fn the_boost_is_given_back_on_unlock() {
    let _k = ts::lock();
    let low = ts::task(40);
    let high = ts::task(5);

    ts::run(low);
    lock(M1);
    ts::run(high);
    lock(M1);
    assert_eq!(ts::prio_of(low), 5);

    ts::run(low);
    unlock(M1);

    assert_eq!(
        ts::prio_of(low),
        40,
        "a boost that outlives the mutex starves everything below it"
    );
    assert_eq!(ts::state_of(high), TaskState::Ready, "the waiter is woken");
}

#[test]
fn ownership_passes_to_the_waiter_not_back_to_free() {
    let _k = ts::lock();
    let a = ts::task(20);
    let b = ts::task(10);

    ts::run(a);
    lock(M1);
    ts::run(b);
    lock(M1);

    ts::run(a);
    unlock(M1);

    assert_eq!(
        owner_of(M1),
        Some(b),
        "releasing to free would let a third task barge in ahead of the one \
         already queued"
    );
}

#[test]
fn the_highest_priority_waiter_is_served_first() {
    let _k = ts::lock();
    let owner = ts::task(40);
    let mid = ts::task(20);
    let top = ts::task(3);

    ts::run(owner);
    lock(M1);
    // `mid` queues first and `top` second, so priority has to beat arrival.
    ts::run(mid);
    lock(M1);
    ts::run(top);
    lock(M1);

    ts::run(owner);
    unlock(M1);

    assert_eq!(owner_of(M1), Some(top), "priority outranks arrival order");
}

#[test]
fn equal_priorities_are_served_in_arrival_order() {
    let _k = ts::lock();
    let owner = ts::task(40);
    let first = ts::task(20);
    let second = ts::task(20);

    ts::run(owner);
    lock(M1);
    ts::run(first);
    lock(M1);
    ts::run(second);
    lock(M1);

    ts::run(owner);
    unlock(M1);

    assert_eq!(
        owner_of(M1),
        Some(first),
        "FIFO among equals, or a busy level starves its own members"
    );
}

#[test]
fn a_boost_survives_releasing_a_different_mutex() {
    let _k = ts::lock();
    let owner = ts::task(40);
    let urgent = ts::task(5);
    let less = ts::task(15);

    ts::run(owner);
    lock(M1);
    lock(M2);

    ts::run(urgent);
    lock(M1);
    ts::run(less);
    lock(M2);
    assert_eq!(ts::prio_of(owner), 5, "boosted by the strongest waiter");

    ts::run(owner);
    unlock(M2);

    assert_eq!(
        ts::prio_of(owner),
        5,
        "recompute must consider every mutex still held, not only the one \
         released -- urgent is still blocked on M1"
    );

    unlock(M1);
    assert_eq!(ts::prio_of(owner), 40, "and now nothing is owed");
}

#[test]
fn inheritance_follows_the_chain_through_a_blocked_owner() {
    let _k = ts::lock();
    let low = ts::task(40);
    let mid = ts::task(20);
    let high = ts::task(5);

    // low holds M2. mid takes M1, then blocks on M2.
    ts::run(low);
    lock(M2);
    ts::run(mid);
    lock(M1);
    lock(M2);
    assert_eq!(ts::prio_of(low), 20, "one link: low inherits from mid");

    // high blocks on M1, held by mid -- who is blocked on M2, held by low.
    ts::run(high);
    lock(M1);

    assert_eq!(ts::prio_of(mid), 5, "the immediate owner is boosted");
    assert_eq!(
        ts::prio_of(low),
        5,
        "and so is the task actually holding everything up. Boosting only mid \
         achieves nothing: mid is blocked, so low is what runs, and at 40 any \
         task between 5 and 40 preempts it while high waits. That is the \
         unbounded inversion inheritance exists to prevent."
    );
}

#[test]
fn a_cycle_in_the_chain_terminates() {
    let _k = ts::lock();
    let a = ts::task(30);
    let b = ts::task(30);
    let high = ts::task(5);

    // Deadlock by construction: a holds M1 and waits on M2, b holds M2 and
    // waits on M1.
    ts::run(a);
    lock(M1);
    ts::run(b);
    lock(M2);
    ts::run(a);
    lock(M2);
    ts::run(b);
    lock(M1);

    // Walking the chain from here has to stop. The deadlock is the caller's
    // bug; spinning inside the kernel looking at it would be ours.
    ts::run(high);
    lock(M1);

    assert!(ts::prio_of(a) <= 30, "boost applied without looping forever");
}

#[test]
fn relocking_your_own_mutex_is_refused_rather_than_deadlocking() {
    let _k = ts::lock();
    let t = ts::task(20);
    ts::run(t);

    assert!(lock(M1));
    assert!(
        !lock(M1),
        "a non-recursive mutex must refuse, not enqueue the owner behind \
         itself -- that deadlocks the task with no diagnostic at all"
    );
    assert_eq!(ts::state_of(t), TaskState::Running, "and it stays runnable");
}

#[test]
fn a_full_waiter_list_leaves_no_phantom_boost() {
    let _k = ts::lock();
    let owner = ts::task(40);
    ts::run(owner);
    lock(M3);

    for _ in 0..MAX_WAITERS {
        let w = ts::task(20);
        ts::run(w);
        assert!(lock(M3));
    }
    let before = ts::prio_of(owner);

    // The next one is rejected, and must not move the owner's priority: there
    // would be no waiter entry to justify it or later to undo it.
    let overflow = ts::task(1);
    ts::run(overflow);
    assert!(!lock(M3), "the waiter list is full");
    assert_eq!(
        ts::prio_of(overflow),
        1,
        "the rejected task keeps its own priority"
    );
    assert_eq!(
        ts::prio_of(owner),
        before,
        "a rejected waiter must not leave the owner permanently boosted"
    );
    assert_eq!(ts::state_of(overflow), TaskState::Running, "and stays runnable");
}

#[test]
fn the_ready_mask_survives_a_whole_inversion_cycle() {
    let _k = ts::lock();
    let low = ts::task(40);
    let high = ts::task(5);

    ts::run(low);
    lock(M1);
    ts::assert_ready_mask_consistent();

    ts::run(high);
    lock(M1); // high blocks, so its level goes clear
    ts::assert_ready_mask_consistent();

    ts::run(low);
    unlock(M1); // high wakes at 5, low drops 5 -> 40
    ts::assert_ready_mask_consistent();
}
