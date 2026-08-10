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
//! All waiter-table access happens inside a critical section.

use crate::arch::cs_with;
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

static mut QUEUE_WAITERS: QueueWaiters = QueueWaiters::new();

fn waiters() -> &'static mut QueueWaiters {
    unsafe { &mut *core::ptr::addr_of_mut!(QUEUE_WAITERS) }
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
        let ok = match waiters().find_or_create(q_addr) {
            Some(l) => push(&mut l.send_waiters, &mut l.send_count, cur),
            None => false,
        };
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
    cs_with(|| {
        if let Some(l) = waiters().find(q_addr) {
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
        let ok = match waiters().find_or_create(q_addr) {
            Some(l) => push(&mut l.recv_waiters, &mut l.recv_count, cur),
            None => false,
        };
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

    cs_with(|| {
        if let Some(l) = waiters().find(q_addr) {
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
        let id = waiters().find(q_addr).and_then(|l| {
            pop_first_blocked(sched, &mut l.recv_waiters, &mut l.recv_count, TaskState::BlockedRecv)
        });
        if let Some(id) = id {
            sched.unblock(id);
        }
    });
}

/// Wake one sender after a successful receive (a slot is now free).
pub fn wake_one_sender(q_addr: usize) {
    scheduler::with(|sched| {
        let id = waiters().find(q_addr).and_then(|l| {
            pop_first_blocked(sched, &mut l.send_waiters, &mut l.send_count, TaskState::BlockedSend)
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
    cs_with(|| {
        let table = waiters();
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
    cs_with(|| {
        let table = waiters();
        (0..table.count as usize).any(|i| {
            let (_, list) = &table.entries[i];
            contains(&list.send_waiters, list.send_count, id)
                || contains(&list.recv_waiters, list.recv_count, id)
        })
    })
}
