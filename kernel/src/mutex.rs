// SPDX-License-Identifier: Apache-2.0

//! Kernel-side priority-inheritance mutex (plan W3.2, W3.5, W4).
//!
//! A higher-priority task blocking on a mutex held by a lower-priority owner
//! boosts the owner to the waiter's priority. On release, the owner's effective
//! priority is recomputed from the highest-priority waiter across *all* mutexes
//! it still holds (so multiple held mutexes compose correctly), and ownership
//! transfers to the next waiter.

use arch_xtensa::cs_with;
use crate::scheduler::{self};

const MAX_MUTEXES: usize = 16;
const MAX_WAITERS: usize = 8;
const NO_TASK: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct MutexEntry {
    addr: usize, // 0 = free slot
    owner: u32,
    waiters: [u32; MAX_WAITERS],
    waiter_count: u32,
}

static mut MUTEXES: [MutexEntry; MAX_MUTEXES] = [MutexEntry {
    addr: 0,
    owner: NO_TASK,
    waiters: [NO_TASK; MAX_WAITERS],
    waiter_count: 0,
}; MAX_MUTEXES];

fn table() -> &'static mut [MutexEntry; MAX_MUTEXES] {
    unsafe { &mut *core::ptr::addr_of_mut!(MUTEXES) }
}

fn find_or_create(addr: usize) -> Option<usize> {
    let t = table();
    for i in 0..MAX_MUTEXES {
        if t[i].addr == addr {
            return Some(i);
        }
    }
    for i in 0..MAX_MUTEXES {
        if t[i].addr == 0 {
            t[i].addr = addr;
            t[i].owner = NO_TASK;
            t[i].waiter_count = 0;
            return Some(i);
        }
    }
    None
}

enum LockOutcome {
    Acquired,
    Blocked,
    Failed,
}

fn log_error(args: core::fmt::Arguments<'_>) {
    crate::debug::log::write(api::debug::log::Level::Error, &args);
}

/// Lock `addr`. Returns true if owned on return (immediately or after blocking
/// and being granted), false if the lock could not even be queued — table
/// full, waiter list full, called from interrupt context, or (see below)
/// called by the task that already owns it (W3.5 — never a silent
/// fake-success, never a silent deadlock).
///
/// # Reentrancy (item 1)
///
/// This is a **non-recursive** mutex. The naive fix for "owner tries to lock
/// again" would be either (a) hand back success immediately (reentrant), or
/// (b) detect it and fail loudly. We deliberately choose (b): nothing in
/// this codebase's `MutexGuard`/`unlock` bookkeeping is written to support
/// nested ownership (an inner `unlock` would release the mutex out from
/// under the still-active outer critical section), so silently granting
/// reentrancy would trade one bug (self-deadlock) for a subtler one (early
/// release while the outer caller still thinks it holds the lock). Failing
/// loudly surfaces the misuse instead of hiding it. The previous behavior —
/// falling through to the "held" branch and enqueuing the owner as its own
/// waiter — deadlocked the task forever with no diagnostic at all.
pub fn lock(addr: usize) -> bool {
    let outcome = cs_with(|| {
        if crate::interrupt::in_interrupt() {
            log_error(format_args!(
                "mutex::lock called from interrupt context (addr={:#x})",
                addr
            ));
            return LockOutcome::Failed;
        }
        let cur = scheduler::global().current;
        let idx = match find_or_create(addr) {
            Some(i) => i,
            None => {
                // Item 12: table exhaustion must be observable, not a silent
                // spin at the API layer.
                log_error(format_args!(
                    "mutex table exhausted (MAX_MUTEXES={}, addr={:#x})",
                    MAX_MUTEXES, addr
                ));
                return LockOutcome::Failed;
            }
        };
        let t = table();
        if t[idx].owner == NO_TASK {
            t[idx].owner = cur;
            return LockOutcome::Acquired;
        }
        if t[idx].owner == cur {
            log_error(format_args!(
                "mutex::lock: task {} re-locked a mutex it already owns (addr={:#x}) — refusing (non-recursive mutex)",
                cur, addr
            ));
            return LockOutcome::Failed;
        }
        // Held by someone else: priority inheritance + queue the waiter.
        //
        // Item 3: check waiter-list capacity *before* boosting the owner's
        // priority. The original order boosted first and checked capacity
        // second, so a waiter rejected for lack of room still left the owner
        // permanently boosted with no corresponding waiter-list entry to
        // justify (or later undo, via `recompute_owner_priority`) it.
        if (t[idx].waiter_count as usize) >= MAX_WAITERS {
            log_error(format_args!(
                "mutex waiter list full (addr={:#x}, MAX_WAITERS={})",
                addr, MAX_WAITERS
            ));
            return LockOutcome::Failed;
        }
        let owner = t[idx].owner;
        let cur_prio = scheduler::global().tasks[cur as usize]
            .as_ref()
            .map_or(scheduler::IDLE_PRIORITY, |x| x.priority);
        scheduler::global().boost_priority(owner, cur_prio);
        let wc = t[idx].waiter_count as usize;
        t[idx].waiters[wc] = cur;
        t[idx].waiter_count += 1;
        scheduler::global().block_current_on_mutex(addr);
        LockOutcome::Blocked
    });

    match outcome {
        LockOutcome::Acquired => true,
        LockOutcome::Failed => false,
        LockOutcome::Blocked => {
            scheduler::request_switch();
            // On resume, ownership has been transferred to us by unlock().
            true
        }
    }
}

/// Unlock `addr`, transferring ownership to the next waiter (if any) and
/// recomputing the releasing owner's inherited priority.
///
/// Item 2: refuses (logs, and in debug builds panics) if the caller is not
/// the current owner instead of blindly transferring ownership by address.
/// Without this check any task — buggy or malicious — could release a
/// mutex it never held, corrupting whichever task's critical section was
/// actually relying on holding it.
pub fn unlock(addr: usize) {
    let switched = cs_with(|| {
        let idx = match table().iter().position(|e| e.addr == addr) {
            Some(i) => i,
            None => return false,
        };
        let cur = scheduler::global().current;
        let t = table();
        if t[idx].owner == NO_TASK {
            log_error(format_args!(
                "mutex::unlock: task {} unlocked a mutex that is not held (addr={:#x})",
                cur, addr
            ));
            #[cfg(debug_assertions)]
            crate::debug::panic::handle(&format_args!(
                "mutex::unlock: double/spurious unlock (addr={:#x}, caller={})",
                addr, cur
            ));
            #[cfg(not(debug_assertions))]
            return false;
        }
        if t[idx].owner != cur {
            log_error(format_args!(
                "mutex::unlock: task {} released mutex @ {:#x} owned by task {}",
                cur, addr, t[idx].owner
            ));
            #[cfg(debug_assertions)]
            crate::debug::panic::handle(&format_args!(
                "mutex::unlock: not the owner (addr={:#x}, owner={}, caller={})",
                addr, t[idx].owner, cur
            ));
            #[cfg(not(debug_assertions))]
            return false;
        }
        let prev_owner = t[idx].owner;

        if t[idx].waiter_count > 0 {
            // Pop the highest-priority waiter (FIFO among equal priorities).
            let next = pop_best_waiter(idx);
            t[idx].owner = next;
            scheduler::global().unblock(next);
        } else {
            t[idx].owner = NO_TASK;
            t[idx].addr = 0; // free the slot
        }

        // Drop boosts this owner received from the mutex(es) it no longer needs.
        recompute_owner_priority(prev_owner);
        true
    });
    if switched {
        // A newly-unblocked higher-priority waiter may need to run now.
        scheduler::request_switch();
    }
}

/// Remove and return the highest-priority (lowest numeric) waiter of mutex
/// `idx`, preserving FIFO order for the rest.
fn pop_best_waiter(idx: usize) -> u32 {
    let t = table();
    let count = t[idx].waiter_count as usize;
    let mut best = 0usize;
    let mut best_prio = u8::MAX;
    for i in 0..count {
        let id = t[idx].waiters[i];
        let prio = scheduler::global().tasks[id as usize]
            .as_ref()
            .map_or(u8::MAX, |x| x.priority);
        if prio < best_prio {
            best_prio = prio;
            best = i;
        }
    }
    let chosen = t[idx].waiters[best];
    for i in best + 1..count {
        t[idx].waiters[i - 1] = t[idx].waiters[i];
    }
    t[idx].waiter_count -= 1;
    chosen
}

/// Recompute `owner`'s effective priority as the strongest of its base priority
/// and the highest-priority waiter across every mutex it still holds.
fn recompute_owner_priority(owner: u32) {
    let base = scheduler::global().base_priority(owner);
    let mut effective = base;
    let t = table();
    for e in t.iter() {
        if e.addr != 0 && e.owner == owner {
            for i in 0..e.waiter_count as usize {
                let id = e.waiters[i];
                let prio = scheduler::global().tasks[id as usize]
                    .as_ref()
                    .map_or(u8::MAX, |x| x.priority);
                if prio < effective {
                    effective = prio;
                }
            }
        }
    }
    scheduler::global().set_inherited_priority(owner, effective);
}
