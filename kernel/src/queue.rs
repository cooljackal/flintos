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

use crate::scheduler::{self, TaskState};

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
    scheduler::request_switch();

    // Resumed: still listed ⇒ timed out; removed ⇒ woken by a slot.
    with_waiters(|w| {
        if let Some(l) = w.find(q_addr) {
            if contains(&l.send_waiters, l.send_count, cur) {
                remove(&mut l.send_waiters, &mut l.send_count, cur);
                return false;
            }
        }
        true
    })
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
    scheduler::request_switch();

    with_waiters(|w| {
        if let Some(l) = w.find(q_addr) {
            if contains(&l.recv_waiters, l.recv_count, cur) {
                remove(&mut l.recv_waiters, &mut l.recv_count, cur);
                return false;
            }
        }
        true
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
