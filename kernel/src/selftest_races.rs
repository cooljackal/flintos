// SPDX-License-Identifier: Apache-2.0

//! Task-versus-ISR race tests. Included by [`crate::selftest`].
//!
//! These are the tests a host cannot run, and saying why is worth the space.
//! `kernel::arch`'s host `cs_with` masks nothing — on a host there is nothing
//! to mask — so a host test that takes a critical section and then checks an
//! invariant is testing an ordinary function call. Two OS threads on x86 are
//! not an ISR preempting a task on one Xtensa core either: the interleavings
//! differ, and x86 reorders far less.
//!
//! So the concurrency the kernel actually has is only observable here.
//!
//! The producer in these tests is real. `timer::every` callbacks are invoked
//! from `process_timers`, which the trap handler calls — so a callback runs in
//! interrupt context, preempting whatever the task was doing. That is the same
//! path a driver's top half takes, not a simulation of it.

use core::sync::atomic::{AtomicU32, Ordering};

use api::queue::Queue;
use hal::tick::TickSource;

use crate::arch::Tick;
use crate::scheduler::{self, TaskState};

use super::{spin_cycles, Check};

// ── ISR-produced queue traffic ──────────────────────────────────────────────

/// Fed from the timer ISR, drained by the task running the self-test.
///
/// Deliberately small. A queue big enough never to fill would test the happy
/// path only; at 16 the consumer has to keep up, and the full-queue rejection
/// path is exercised on the same run.
static ISR_Q: Queue<u32, 16> = Queue::new();

/// Next value the ISR will try to send. Also the count of attempts.
static ISR_NEXT: AtomicU32 = AtomicU32::new(0);
/// Sends the ISR completed. The consumer must see exactly this many.
static ISR_SENT: AtomicU32 = AtomicU32::new(0);

/// Runs in interrupt context, from the trap handler.
fn isr_producer() {
    let v = ISR_NEXT.fetch_add(1, Ordering::Relaxed);
    // `try_send` rather than `send_isr`: a full queue is expected here and the
    // rejection is part of what is being tested. Waking a receiver that is not
    // blocked would only add scheduler work to a path already under test.
    if ISR_Q.try_send(v).is_ok() {
        ISR_SENT.fetch_add(1, Ordering::Relaxed);
    }
}

/// A queue fed from the timer ISR and drained by a task loses nothing and
/// duplicates nothing.
///
/// This is the race the `Queue` slot state machine exists for, against the
/// concurrency it was written for rather than against threads. The ISR really
/// does interrupt the consumer — including, sometimes, between the consumer's
/// `head` CAS and its read of the payload, which is precisely the window that
/// the older count-only design got wrong.
///
/// Checked by value, not just by count: a single producer sends strictly
/// increasing values, so the consumer must observe them strictly increasing. A
/// value out of order means a slot was published before its payload landed, or
/// handed to two readers.
pub fn isr_queue_delivers_exactly_once() -> Check {
    ISR_NEXT.store(0, Ordering::Relaxed);
    ISR_SENT.store(0, Ordering::Relaxed);
    while ISR_Q.try_recv().is_ok() {} // start from empty

    let id = crate::timer::every(1, isr_producer);

    let mut received = 0u32;
    let mut last: Option<u32> = None;
    let deadline = Tick::now().saturating_add(200);

    while Tick::now() < deadline {
        while let Ok(v) = ISR_Q.try_recv() {
            if let Some(prev) = last {
                if v <= prev {
                    crate::timer::cancel(id);
                    return Err("ISR-produced values arrived out of order");
                }
            }
            last = Some(v);
            received += 1;
        }
        core::hint::spin_loop();
    }

    // Stop the producer before the final accounting. Once `cancel` returns from
    // task context the callback is out of the table, and on one core that means
    // no further send can be in flight.
    crate::timer::cancel(id);
    while let Ok(v) = ISR_Q.try_recv() {
        if let Some(prev) = last {
            if v <= prev {
                return Err("ISR-produced values arrived out of order while draining");
            }
        }
        last = Some(v);
        received += 1;
    }

    if received == 0 {
        return Err("the timer ISR never produced anything -- is the tick running?");
    }
    if received != ISR_SENT.load(Ordering::Relaxed) {
        return Err("received count differs from what the ISR sent");
    }
    Ok(())
}

// ── Critical sections ───────────────────────────────────────────────────────

/// Leaving an inner critical section must not unmask the outer one.
///
/// The bug this catches is a `cs_with` that unmasks unconditionally on exit
/// instead of restoring the interrupt level it found. With that mistake the
/// outer section keeps running with interrupts on, believing itself protected —
/// and every invariant it was holding becomes racy without anything looking
/// wrong at the call site.
///
/// `critical_section_masks_the_tick` cannot see this: it only ever nests one
/// deep.
pub fn nested_critical_sections_stay_masked() -> Check {
    let per = Tick::ticks_per_period();

    crate::arch::cs_with(|| {
        let before = Tick::now();

        crate::arch::cs_with(|| {
            spin_cycles(per.saturating_mul(2));
        });

        // Still inside the outer section. If the inner exit unmasked, the tick
        // moves during this spin.
        spin_cycles(per.saturating_mul(3));

        if Tick::now() != before {
            return Err("leaving the inner critical section unmasked the outer one");
        }
        Ok(())
    })
}

/// After a tick has been serviced, task context must not still look like
/// interrupt context.
///
/// `interrupt::in_interrupt()` gates `mutex::lock`, which refuses to block from
/// an ISR. A depth counter that leaks — an early return past the guard, a
/// mismatched enter/exit — leaves every later `lock()` failing for a reason
/// that has nothing to do with the caller.
pub fn interrupt_depth_returns_to_zero() -> Check {
    if crate::interrupt::in_interrupt() {
        return Err("already in interrupt context before the test started");
    }

    // Ticks first. This part is nearly free and proves nothing on its own --
    // see below -- but a depth left dirty by some future tick-path change
    // would show up here.
    spin_cycles(Tick::ticks_per_period().saturating_mul(5));
    if crate::interrupt::in_interrupt() {
        return Err("interrupt depth did not return to zero after servicing ticks");
    }

    // The guard's own contract, exercised directly.
    //
    // This test used to be only the tick check above, and a mutation that
    // removed the decrement from `InterruptGuard::drop` did not fail it. The
    // reason is that `InterruptGuard` is entered only in `interrupt::dispatch`,
    // which handles *routed peripheral* IRQs -- the tick and the software
    // interrupt do not go through it. With no peripheral interrupt firing
    // during the suite, the depth was never incremented, so a broken decrement
    // was invisible. The test was measuring nothing.
    {
        let _outer = crate::interrupt::InterruptGuard::enter();
        if !crate::interrupt::in_interrupt() {
            return Err("entering interrupt context was not visible to in_interrupt");
        }
        {
            let _inner = crate::interrupt::InterruptGuard::enter();
            if !crate::interrupt::in_interrupt() {
                return Err("a nested interrupt guard lost interrupt context");
            }
        }
        // The inner guard has gone; the outer one has not.
        if !crate::interrupt::in_interrupt() {
            return Err("leaving a nested guard left interrupt context too early");
        }
    }
    if crate::interrupt::in_interrupt() {
        return Err("interrupt depth did not return to zero after the guards were dropped");
    }
    Ok(())
}

// ── Scheduler state under interruption ──────────────────────────────────────

/// The ready mask must agree with the task states, sampled repeatedly while
/// ticks are landing.
///
/// The mask is a cache of "is anyone runnable at this priority", maintained
/// incrementally by every block, unblock and priority change. The trap handler
/// updates it too, from the tick, so a read-modify-write in task context that
/// is not actually atomic can lose an update — and the two ways it breaks fail
/// very differently. A bit set with nothing behind it makes `schedule()` pick a
/// task that is not runnable; a bit clear with a Ready task behind it strands
/// that task forever, with its TCB still claiming it is Ready and nothing
/// anywhere reporting a problem.
///
/// Sampling under a critical section is deliberate: it checks that the state
/// the trap handler leaves behind is consistent, not that it is consistent
/// mid-update.
pub fn ready_mask_agrees_with_task_states() -> Check {
    for _ in 0..2_000 {
        let bad = crate::arch::cs_with(|| {
            let sched = scheduler::global();
            for prio in 0..scheduler::NUM_PRIORITIES as u8 {
                let runnable = sched.tasks.iter().flatten().any(|t| {
                    t.priority == prio
                        && matches!(t.state, TaskState::Ready | TaskState::Running)
                });
                let bit = sched.ready_mask & (1u64 << prio) != 0;
                if bit != runnable {
                    return true;
                }
            }
            false
        });
        if bad {
            return Err("ready_mask disagrees with the task states after a tick");
        }
        core::hint::spin_loop();
    }
    Ok(())
}

/// Exactly one consumer may observe a pending switch.
///
/// `take_pending_switch` is a swap, and the trap handler calls it on every
/// trap. If it degraded to a load-then-store, a tick landing between the two
/// would let the same switch be consumed twice — the second consumer switching
/// on a request that had already been served, which on a busy system means a
/// task loses its slice for no reason anyone can trace.
///
/// Setting and immediately taking, thousands of times with ticks landing
/// throughout, means the trap handler and this loop are contending for the same
/// flag continuously.
pub fn pending_switch_is_taken_once() -> Check {
    let mut taken = 0u32;
    for _ in 0..5_000 {
        scheduler::set_pending_switch();
        if scheduler::take_pending_switch() {
            taken += 1;
        }
        // A second take must never succeed: nothing set it again, and the trap
        // handler only ever consumes.
        if scheduler::take_pending_switch() {
            return Err("a pending switch was consumed twice");
        }
    }
    if taken == 0 {
        return Err("set_pending_switch never became visible to take_pending_switch");
    }
    Ok(())
}

/// A mutex taken and released across ticks leaves nothing behind.
///
/// Uncontended by construction — there is one task running at this point in
/// boot — so this is not an inversion test. What it exercises is the
/// bookkeeping under interruption: `lock` and `unlock` each do a
/// read-modify-write of the mutex table and the owner's priority inside a
/// critical section, and a tick landing mid-sequence must not leave the slot
/// occupied, the owner boosted, or the table leaking entries.
///
/// Priority inversion itself is covered on the host, where the scenarios can be
/// built exactly; see `kernel/src/mutex_tests.rs`.
pub fn mutex_cycle_under_ticks_leaves_no_residue() -> Check {
    const ADDR: usize = 0xF1E7_0001;

    let base = crate::arch::cs_with(|| {
        let sched = scheduler::global();
        let cur = sched.current;
        sched.base_priority(cur)
    });

    for _ in 0..1_000 {
        if !crate::mutex::lock(ADDR) {
            return Err("an uncontended mutex refused to lock");
        }
        crate::mutex::unlock(ADDR);
    }

    let effective = crate::arch::cs_with(|| {
        let sched = scheduler::global();
        let cur = sched.current;
        sched.tasks[cur as usize]
            .as_ref()
            .map_or(u8::MAX, |t| t.priority)
    });

    if effective != base {
        return Err("priority did not return to base after uncontended lock/unlock");
    }

    // The slot must be free again, or 1000 cycles would have exhausted a table
    // of 16.
    if !crate::mutex::lock(ADDR) {
        return Err("the mutex table leaked entries across lock/unlock cycles");
    }
    crate::mutex::unlock(ADDR);

    // Now with a boost to give back.
    //
    // Everything above is uncontended, so no priority inheritance ever
    // happens -- which meant a mutation deleting `recompute_owner_priority`
    // from `unlock` did not fail this test. There was no boost to fail to
    // restore. The check was real but vacuous.
    //
    // Staging a genuine waiter would need a second task blocking on this
    // mutex, and the suite runs at idle priority where that is awkward. The
    // boost is applied directly instead: this asserts that `unlock` gives a
    // boost back, which is the part that runs on target. That contention
    // *causes* a boost is covered by the host tests, including through chains
    // of blocked owners.
    let cur = crate::arch::cs_with(|| scheduler::global().current);
    let boosted = base.saturating_sub(1);
    if boosted == base {
        // Already at the top of the range; there is no higher priority to be
        // boosted to, so skip rather than assert something meaningless.
        return Ok(());
    }

    if !crate::mutex::lock(ADDR) {
        return Err("could not take the mutex for the boost check");
    }
    crate::arch::cs_with(|| scheduler::global().boost_priority(cur, boosted));

    let while_held = crate::arch::cs_with(|| {
        scheduler::global().tasks[cur as usize]
            .as_ref()
            .map_or(u8::MAX, |t| t.priority)
    });
    if while_held != boosted {
        crate::mutex::unlock(ADDR);
        return Err("boost_priority did not raise the holder's priority");
    }

    crate::mutex::unlock(ADDR);

    let after = crate::arch::cs_with(|| {
        scheduler::global().tasks[cur as usize]
            .as_ref()
            .map_or(u8::MAX, |t| t.priority)
    });
    if after != base {
        return Err("unlock did not give back the priority the holder was boosted to");
    }
    Ok(())
}
