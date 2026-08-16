// SPDX-License-Identifier: Apache-2.0

//! Kernel-side queue waiter management (plan W4).
//!
//! When a task blocks on a full queue (send) or empty queue (recv), it is
//! recorded in a per-queue waiter list keyed by the queue's address. A
//! successful send wakes one receiver; a successful receive wakes one sender.
//! Finite timeouts are handled cooperatively with the scheduler: a timed-out
//! task is woken by the tick and discovers the timeout because it is still in
//! the waiter list on resume (`block_*` returns false).
//!
//! All waiter-table access happens under one lock, `QUEUE_WAITERS`.

use crate::scheduler::{self, TaskState, MAX_TASKS};

const MAX_WAITERS: usize = 16;
const MAX_QUEUES: usize = 16;
const NO_TASK: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct WaiterList {
    send_waiters: [u32; MAX_WAITERS],
    recv_waiters: [u32; MAX_WAITERS],
    send_count: u32,
    recv_count: u32,
}

impl WaiterList {
    const fn new() -> Self {
        Self {
            send_waiters: [NO_TASK; MAX_WAITERS],
            recv_waiters: [NO_TASK; MAX_WAITERS],
            send_count: 0,
            recv_count: 0,
        }
    }
}

fn push(list: &mut [u32; MAX_WAITERS], count: &mut u32, id: u32) -> bool {
    if (*count as usize) < list.len() {
        list[*count as usize] = id;
        *count += 1;
        true
    } else {
        false
    }
}

/// Pop the first waiter still actually blocked in `expected` state, leaving
/// any earlier entries the tick has already timed out untouched (item 5).
///
/// The tick (`Scheduler::on_tick`) marks a timed-out waiter `Ready` *without*
/// removing it from this list — by design, per the module docs, it detects
/// its own timeout on resume by finding itself still listed. If we popped
/// unconditionally here (the old `pop`), we could hand a timed-out task's
/// spot to nobody (it silently vanishes from the list, so on resume it no
/// longer finds itself listed and wrongly concludes it was woken normally —
/// stealing a delivery it never received) while the *real* waiter behind it
/// in FIFO order never gets woken at all. Skipping stale entries in place
/// preserves both: the timed-out task still finds itself listed later, and
/// the real waiter is the one actually returned here.
fn pop_first_blocked(
    sched: &mut scheduler::Scheduler,
    list: &mut [u32; MAX_WAITERS],
    count: &mut u32,
    expected: TaskState,
) -> Option<u32> {
    let n = *count as usize;
    for i in 0..n {
        let id = list[i];
        let still_waiting = sched.tasks[id as usize]
            .as_ref()
            .is_some_and(|t| t.state == expected);
        if still_waiting {
            for k in i + 1..n {
                list[k - 1] = list[k];
            }
            *count -= 1;
            return Some(id);
        }
        // Stale (already timed out, or otherwise no longer waiting): leave
        // it in the list and keep scanning.
    }
    None
}

fn contains(list: &[u32; MAX_WAITERS], count: u32, id: u32) -> bool {
    list[..count as usize].contains(&id)
}

fn remove(list: &mut [u32; MAX_WAITERS], count: &mut u32, id: u32) {
    if let Some(pos) = list[..*count as usize].iter().position(|&x| x == id) {
        for i in pos + 1..*count as usize {
            list[i - 1] = list[i];
        }
        *count -= 1;
    }
}

struct QueueWaiters {
    entries: [(usize, WaiterList); MAX_QUEUES],
    count: u32,
}

impl QueueWaiters {
    const fn new() -> Self {
        Self {
            entries: [(0, WaiterList::new()); MAX_QUEUES],
            count: 0,
        }
    }

    /// Find an existing waiter list, or create one. Returns None if the table
    /// is full (W3.5: surfaced, not panicked).
    fn find_or_create(&mut self, q_addr: usize) -> Option<&mut WaiterList> {
        for i in 0..self.count as usize {
            if self.entries[i].0 == q_addr {
                return Some(&mut self.entries[i].1);
            }
        }
        if (self.count as usize) < self.entries.len() {
            let idx = self.count as usize;
            self.entries[idx] = (q_addr, WaiterList::new());
            self.count += 1;
            Some(&mut self.entries[idx].1)
        } else {
            None
        }
    }

    fn find(&mut self, q_addr: usize) -> Option<&mut WaiterList> {
        self.entries[..self.count as usize]
            .iter_mut()
            .find(|(a, _)| *a == q_addr)
            .map(|(_, l)| l)
    }
}

/// The waiter table, behind a lock that excludes the other core.
///
/// This was a bare `static mut` reached through a `waiters()` accessor, and it
/// was reached under **two different locks**: the paths that also touch the
/// scheduler took it inside `scheduler::with`, while the resume path,
/// `forget_task` and `is_waiting_anywhere` took only `cs_with`. A critical
/// section is `rsil`, which masks the calling core alone, so those two groups
/// did not exclude each other at all — core 0 resuming from a block could
/// rewrite a list while core 1 was pushing to it inside the scheduler lock.
/// One lock, taken by every path, is the fix.
///
/// **Lock order: scheduler, then this.** The three sites that need both nest
/// in that order; nothing takes the scheduler while holding this. Reversing it
/// anywhere deadlocks a two-core board.
static QUEUE_WAITERS: crate::smp::Spinlock<QueueWaiters> =
    crate::smp::Spinlock::new(QueueWaiters::new());

/// Run `f` with the waiter table locked.
fn with_waiters<R>(f: impl FnOnce(&mut QueueWaiters) -> R) -> R {
    QUEUE_WAITERS.with(f)
}

/// Deadline for a timeout: 0 = forever (never tick-woken); else now+timeout
/// (never 0, so it is distinguishable from "forever").
/// Absolute tick a timeout expires at, given the current tick.
///
/// `now` is passed in rather than read here, and that is the whole point.
/// This used to call `scheduler::with` to read the tick — from inside
/// `block_send`/`block_recv`, which were already holding that lock. Since the
/// scheduler became a `Spinlock`, taking it twice on one core is a panic
/// rather than a wait, so **every blocking send or receive on a full or empty
/// queue panicked**. The queue's own tests never caught it because they
/// exercise `try_send`/`try_recv`, which never block.
///
/// `0` means "no timeout"; the tick never wakes such a waiter.
fn deadline_for(timeout_ms: u32, now: u64) -> u64 {
    if timeout_ms == u32::MAX {
        0
    } else {
        now.wrapping_add(timeout_ms as u64).max(1)
    }
}

/// Absolute deadline for a retry loop, on the same clock `block_send` and
/// `block_recv` arm their timeouts against. `0` means "forever".
///
/// A retry loop must not pass its original `timeout_ms` to `block_*` each time
/// round: `deadline_for` computes a fresh `now + timeout_ms` on every call, so
/// re-passing it re-arms the full wait, and under repeated spurious wakeups the
/// *total* wait is unbounded even though the caller asked for a bounded one.
/// `api::queue` learned this first (item 9); the pair lives here so that the
/// next caller inherits the fix rather than the bug.
pub fn retry_deadline(timeout_ms: u32) -> u64 {
    deadline_for(timeout_ms, scheduler::with(|s| s.ticks()))
}

/// Time left before `deadline`, to pass to the next `block_*` call.
/// `None` means it has passed and the caller should report a timeout.
pub fn retry_remaining(deadline: u64) -> Option<u32> {
    remaining_for(deadline, scheduler::with(|s| s.ticks()))
}

/// The arithmetic half of [`retry_remaining`], split out so it is testable
/// without a scheduler. `0` is the "forever" sentinel [`deadline_for`] returns.
fn remaining_for(deadline: u64, now: u64) -> Option<u32> {
    if deadline == 0 {
        return Some(u32::MAX);
    }
    if now >= deadline {
        None
    } else {
        Some((deadline - now).min(u32::MAX as u64) as u32)
    }
}

/// Block the caller waiting to send on a full queue.
/// Returns true if woken by an opening slot (caller should retry), false on
/// timeout, if the waiter table is full, or (item 11) if called from
/// interrupt context — blocking there would suspend the interrupted task,
/// not the caller, wedging it forever, so we refuse instead.
pub fn block_send(q_addr: usize, timeout_ms: u32) -> bool {
    if crate::interrupt::in_interrupt() {
        crate::debug::log::write(
            api::debug::log::Level::Error,
            &format_args!("queue::block_send called from interrupt context (q={:#x})", q_addr),
        );
        return false;
    }
    // Outside the lock below: `deadline_for` needs the tick, and reading it
    // through `scheduler::with` while already inside one is the reentrancy
    // that panicked.
    let now = scheduler::with(|s| s.ticks());
    let cur = scheduler::with(|sched| {
        let cur = sched.current();
        let dl = deadline_for(timeout_ms, now);
        if let Some(tcb) = &mut sched.tasks[cur as usize] {
            tcb.sleep_until = dl;
        }
        let ok = with_waiters(|w| match w.find_or_create(q_addr) {
            Some(l) => push(&mut l.send_waiters, &mut l.send_count, cur),
            None => false,
        });
        if ok {
            sched.block_current(TaskState::BlockedSend);
            Some(cur)
        } else {
            None
        }
    });
    let cur = match cur {
        Some(c) => c,
        None => return false, // table/list full — cannot block
    };
    note_wait(cur, Wait { addr: q_addr, send: true, timeout_ms, since: now });
    scheduler::request_switch();

    // Resumed: still listed ⇒ timed out; removed ⇒ woken by a slot.
    let woken = with_waiters(|w| {
        if let Some(l) = w.find(q_addr) {
            if contains(&l.send_waiters, l.send_count, cur) {
                remove(&mut l.send_waiters, &mut l.send_count, cur);
                return false;
            }
        }
        true
    });
    clear_wait(cur);
    woken
}

/// What a task is blocked on, and since when.
///
/// Every blocking primitive the blob is given — queue send and receive,
/// semaphore take, mutex lock — funnels through [`block_send`] and
/// [`block_recv`], so recording here covers all four. It exists because a wait
/// that is never signalled looks exactly like a dead system from outside: the
/// task is simply not runnable, and nothing says which object it is holding
/// out for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wait {
    /// The object's address, which is the key the waiter list is filed under.
    pub addr: usize,
    /// True for a send (waiting for space), false for a receive (waiting for
    /// an item, a permit, or an unlock).
    pub send: bool,
    /// What the caller asked for. [`dynobj::FOREVER`](crate::dynobj::FOREVER)
    /// is `u32::MAX`.
    pub timeout_ms: u32,
    /// The tick the wait began on.
    pub since: u64,
}

static WAITS: crate::smp::Spinlock<[Option<Wait>; MAX_TASKS]> =
    crate::smp::Spinlock::new([None; MAX_TASKS]);

fn note_wait(task: u32, w: Wait) {
    let i = task as usize;
    if i < MAX_TASKS {
        WAITS.with(|t| t[i] = Some(w));
    }
}

fn clear_wait(task: u32) {
    let i = task as usize;
    if i < MAX_TASKS {
        WAITS.with(|t| t[i] = None);
    }
}

/// What `task` is blocked on, if it is blocked in a queue primitive.
///
/// Stale by design in one direction only: it is cleared on the way out, so a
/// task that is running reports `None`, and a task that reports a wait really
/// is in one.
pub fn waiting_on(task: u32) -> Option<Wait> {
    let i = task as usize;
    if i >= MAX_TASKS {
        return None;
    }
    WAITS.with(|t| t[i])
}

/// Block the caller waiting to receive on an empty queue. See [`block_send`].
pub fn block_recv(q_addr: usize, timeout_ms: u32) -> bool {
    if crate::interrupt::in_interrupt() {
        crate::debug::log::write(
            api::debug::log::Level::Error,
            &format_args!("queue::block_recv called from interrupt context (q={:#x})", q_addr),
        );
        return false;
    }
    // Outside the lock below: `deadline_for` needs the tick, and reading it
    // through `scheduler::with` while already inside one is the reentrancy
    // that panicked.
    let now = scheduler::with(|s| s.ticks());
    let cur = scheduler::with(|sched| {
        let cur = sched.current();
        let dl = deadline_for(timeout_ms, now);
        if let Some(tcb) = &mut sched.tasks[cur as usize] {
            tcb.sleep_until = dl;
        }
        let ok = with_waiters(|w| match w.find_or_create(q_addr) {
            Some(l) => push(&mut l.recv_waiters, &mut l.recv_count, cur),
            None => false,
        });
        if ok {
            sched.block_current(TaskState::BlockedRecv);
            Some(cur)
        } else {
            None
        }
    });
    let cur = match cur {
        Some(c) => c,
        None => return false,
    };
    note_wait(cur, Wait { addr: q_addr, send: false, timeout_ms, since: now });
    scheduler::request_switch();

    let woken = with_waiters(|w| {
        if let Some(l) = w.find(q_addr) {
            if contains(&l.recv_waiters, l.recv_count, cur) {
                remove(&mut l.recv_waiters, &mut l.recv_count, cur);
                return false;
            }
        }
        true
    });
    clear_wait(cur);
    woken
}

/// Test a resource and enrol as a waiter **without a gap between the two**.
///
/// # The bug this exists to remove
///
/// Every blocking primitive here used to be written as "try, and if that
/// fails, block": `Semaphore::take` called `try_take`, which took its own
/// look at the count and released whatever it held, and only then called
/// [`block_recv`] to join the waiter list. A producer landing in that gap
/// makes the resource available and wakes *nobody*, because there is nobody
/// enrolled yet — and the consumer then blocks on a resource that is already
/// there. With a finite timeout that is a stall; with an infinite one, which
/// is what the radio driver asks for, it never ends.
///
/// It is not theoretical: the driver's start-up hung on a semaphore that had
/// been given exactly once and taken exactly once.
///
/// `ready` runs while this holds the scheduler and the waiter table, which is
/// the same pair the wake path takes and in the same order. So a producer
/// cannot run between the test and the enrolment: it is waiting for one of
/// these two locks.
///
/// Returns `true` when `ready` claimed the resource — the caller has it and
/// did not block. `false` means it blocked and has since been woken or timed
/// out, and should test again.
///
/// `ready` must not take either lock itself, and must not block. It is a
/// count decrement or a byte copy, nothing more.
pub fn consume_or_block(q_addr: usize, timeout_ms: u32, ready: impl FnOnce() -> bool) -> bool {
    test_or_enrol(q_addr, timeout_ms, false, false, ready)
}

/// As [`consume_or_block`], and wake one sender when the test succeeds.
///
/// A queue receive frees a slot, so whoever was waiting for space should be
/// told — under the same locks, for the same reason.
pub fn consume_or_block_waking_sender(
    q_addr: usize,
    timeout_ms: u32,
    ready: impl FnOnce() -> bool,
) -> bool {
    test_or_enrol(q_addr, timeout_ms, false, true, ready)
}

/// The producer's mirror: try to make something available, and if that fails
/// join the queue of those waiting for room.
///
/// On success one receiver is woken, because an item has appeared.
pub fn produce_or_block(
    q_addr: usize,
    timeout_ms: u32,
    produce: impl FnOnce() -> bool,
) -> bool {
    test_or_enrol(q_addr, timeout_ms, true, true, produce)
}

/// The body all four share.
///
/// `sending` picks which waiter list this task joins when the test fails, and
/// which state it blocks in. `wake_other` wakes one waiter from the *opposite*
/// list when the test succeeds — a receive frees a slot, a send provides an
/// item, and a semaphore does neither.
fn test_or_enrol(
    q_addr: usize,
    timeout_ms: u32,
    sending: bool,
    wake_other: bool,
    ready: impl FnOnce() -> bool,
) -> bool {
    if crate::interrupt::in_interrupt() {
        crate::debug::log::write(
            api::debug::log::Level::Error,
            &format_args!("queue::consume_or_block called from interrupt context (q={:#x})", q_addr),
        );
        // Still give it its chance: a try-style call from an interrupt is
        // legal, it just cannot block afterwards.
        return ready();
    }
    let now = scheduler::with(|s| s.ticks());
    let mut to_wake = None;
    let blocked = scheduler::with(|sched| {
        let r = with_waiters(|w| {
            if ready() {
                if wake_other {
                    to_wake = w.find(q_addr).and_then(|l| {
                        if sending {
                            pop_first_blocked(
                                sched,
                                &mut l.recv_waiters,
                                &mut l.recv_count,
                                TaskState::BlockedRecv,
                            )
                        } else {
                            pop_first_blocked(
                                sched,
                                &mut l.send_waiters,
                                &mut l.send_count,
                                TaskState::BlockedSend,
                            )
                        }
                    });
                }
                return None;
            }
            let cur = sched.current();
            let dl = deadline_for(timeout_ms, now);
            if let Some(tcb) = &mut sched.tasks[cur as usize] {
                tcb.sleep_until = dl;
            }
            let listed = match w.find_or_create(q_addr) {
                Some(l) => {
                    if sending {
                        push(&mut l.send_waiters, &mut l.send_count, cur)
                    } else {
                        push(&mut l.recv_waiters, &mut l.recv_count, cur)
                    }
                }
                None => false,
            };
            if listed {
                sched.block_current(if sending {
                    TaskState::BlockedSend
                } else {
                    TaskState::BlockedRecv
                });
                Some(cur)
            } else {
                // The table is full. Not blocking is the honest answer: the
                // caller loops and tries again rather than sleeping through a
                // wakeup that could never reach it.
                None
            }
        });
        if let Some(id) = to_wake {
            sched.unblock(id);
        }
        r
    });
    let Some(cur) = blocked else {
        // Either `ready` claimed it, or there was no room to wait. The caller
        // re-tests either way, and `ready` reports which by its own effect.
        return true;
    };
    note_wait(cur, Wait { addr: q_addr, send: sending, timeout_ms, since: now });
    scheduler::request_switch();
    with_waiters(|w| {
        if let Some(l) = w.find(q_addr) {
            if sending {
                if contains(&l.send_waiters, l.send_count, cur) {
                    remove(&mut l.send_waiters, &mut l.send_count, cur);
                }
            } else if contains(&l.recv_waiters, l.recv_count, cur) {
                remove(&mut l.recv_waiters, &mut l.recv_count, cur);
            }
        }
    });
    clear_wait(cur);
    false
}

/// Make a resource available and wake one waiter, with no gap between.
///
/// The producer half of [`consume_or_block`]. `produce` runs under the same
/// two locks, so a consumer testing the resource either sees it before this
/// runs — and enrols — or after, and takes it. There is no third case.
///
/// `produce` returns whether it actually made anything available; a
/// semaphore already at its ceiling has not, and waking for it would be a
/// spurious wakeup rather than a delivery.
pub fn produce_and_wake(q_addr: usize, produce: impl FnOnce() -> bool) -> bool {
    scheduler::with(|sched| {
        let (made, id) = with_waiters(|w| {
            let made = produce();
            let id = if made {
                w.find(q_addr).and_then(|l| {
                    pop_first_blocked(
                        sched,
                        &mut l.recv_waiters,
                        &mut l.recv_count,
                        TaskState::BlockedRecv,
                    )
                })
            } else {
                None
            };
            (made, id)
        });
        if let Some(id) = id {
            sched.unblock(id);
        }
        made
    })
}

/// Wake one receiver after a successful send (a message is now available).
pub fn wake_one_receiver(q_addr: usize) {
    scheduler::with(|sched| {
        let id = with_waiters(|w| {
            w.find(q_addr).and_then(|l| {
                pop_first_blocked(
                    sched,
                    &mut l.recv_waiters,
                    &mut l.recv_count,
                    TaskState::BlockedRecv,
                )
            })
        });
        if let Some(id) = id {
            sched.unblock(id);
        }
    });
}

/// Wake one sender after a successful receive (a slot is now free).
pub fn wake_one_sender(q_addr: usize) {
    scheduler::with(|sched| {
        let id = with_waiters(|w| {
            w.find(q_addr).and_then(|l| {
                pop_first_blocked(
                    sched,
                    &mut l.send_waiters,
                    &mut l.send_count,
                    TaskState::BlockedSend,
                )
            })
        });
        if let Some(id) = id {
            sched.unblock(id);
        }
    });
}

/// Remove a task from every waiter list it appears in.
///
/// Called when a task is deleted. Without this, a deleted task's id stays
/// listed and a later `wake_one_*` unblocks it — by which time the slot may
/// belong to a *different* task, which is then woken from a wait it never
/// entered. The bug that produces is a task returning from `recv` with nothing
/// received, arbitrarily far from the delete that caused it.
pub fn forget_task(id: u32) {
    with_waiters(|table| {
        for i in 0..table.count as usize {
            let (_, list) = &mut table.entries[i];
            remove(&mut list.send_waiters, &mut list.send_count, id);
            remove(&mut list.recv_waiters, &mut list.recv_count, id);
        }
    });
}

/// Whether a task is listed as waiting anywhere. Test support for
/// [`forget_task`].
pub fn is_waiting_anywhere(id: u32) -> bool {
    with_waiters(|table| {
        (0..table.count as usize).any(|i| {
            let (_, list) = &table.entries[i];
            contains(&list.send_waiters, list.send_count, id)
                || contains(&list.recv_waiters, list.recv_count, id)
        })
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retry_loop_does_not_re_arm_the_full_timeout() {
        // The bug this pair exists to prevent: a caller asks for 10 ms, is
        // woken spuriously at 4 ms and again at 7 ms, and each retry must ask
        // for what is *left* rather than for another 10 ms.
        let deadline = deadline_for(10, 100);
        assert_eq!(remaining_for(deadline, 104), Some(6));
        assert_eq!(remaining_for(deadline, 107), Some(3));
        assert_eq!(remaining_for(deadline, 110), None, "expired, so no further block");
        assert_eq!(remaining_for(deadline, 999), None, "and it stays expired");
    }

    #[test]
    fn forever_stays_forever_across_retries() {
        // `deadline_for` maps u32::MAX to the 0 sentinel; a retry loop must
        // keep asking for u32::MAX rather than reading 0 as "already expired".
        let deadline = deadline_for(u32::MAX, 100);
        assert_eq!(deadline, 0);
        assert_eq!(remaining_for(deadline, 100), Some(u32::MAX));
        assert_eq!(remaining_for(deadline, u64::MAX), Some(u32::MAX));
    }

    #[test]
    fn a_zero_timeout_never_blocks() {
        // `deadline_for(0, now)` is `now` (or 1), so the first `remaining_for`
        // already reports expiry and the caller returns without blocking.
        let deadline = deadline_for(0, 100);
        assert_eq!(remaining_for(deadline, 100), None);
    }

    #[test]
    fn a_deadline_beyond_u32_is_clamped_not_truncated() {
        // The tick is u64 and the kernel call takes u32. Clamping keeps a long
        // wait long; truncating would silently turn it into a short one.
        let deadline = deadline_for(u32::MAX - 1, 0);
        assert_eq!(remaining_for(deadline, 0), Some(u32::MAX - 1));
    }
}
