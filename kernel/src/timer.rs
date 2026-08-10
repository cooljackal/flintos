// SPDX-License-Identifier: Apache-2.0

//! Timer subsystem: sleep and one-shot/periodic callbacks (plan W1, W3.5).
//!
//! `sleep_ms` blocks the current task until the target tick, then requests a
//! switch via the software interrupt. `once`/`every` register software timers
//! whose callbacks fire from the trap handler. Software-timer state is guarded
//! by a critical section against the trap handler.

use crate::scheduler::{self, TaskState};

/// Sleep the current task for `ms` milliseconds.
///
/// Blocking, so it must never be called from trap context (a top-half ISR or
/// a timer callback) — that would suspend whichever task was interrupted,
/// not "the caller", wedging it forever (item 11). `sleep_ms` has no way to
/// return an error to its `()`-returning caller, and silently returning
/// without sleeping would be exactly the "plausible-looking wrong result"
/// the project's philosophy forbids, so misuse is a loud panic instead.
pub fn sleep_ms(ms: u32) {
    if crate::interrupt::in_interrupt() {
        crate::debug::panic::handle(&format_args!(
            "timer::sleep_ms called from interrupt context"
        ));
    }
    scheduler::with(|sched| {
        let cur = sched.current();
        let wake = sched.ticks().wrapping_add(ms as u64);
        if let Some(tcb) = &mut sched.tasks[cur as usize] {
            tcb.sleep_until = wake;
        }
        sched.block_current(TaskState::BlockedSleep);
    });
    scheduler::request_switch();
}

pub struct TimerEntry {
    pub id: u32,
    pub fire_at: u64,
    pub interval: u32, // 0 = one-shot, >0 = periodic
    pub callback: Option<fn()>,
}

const MAX_TIMERS: usize = 16;

/// The timer table and its id counter, behind a lock that excludes the other
/// core.
///
/// These were bare `static mut`s. `register` and `cancel` took `cs_with`,
/// which masks the calling core only, and `process_timers` took **nothing at
/// all** — it ran from the trap handler and relied on interrupts already being
/// masked, which is a one-core argument. Both cores take the tick and both run
/// tasks that call `once`/`every`, so core 0 firing a timer raced core 1
/// registering one.
///
/// **The lock is not held across a callback.** `process_timers` snapshots a
/// due entry, releases, calls back, then re-acquires and re-checks the id —
/// the shape the old code already had for a different reason (a callback may
/// cancel its own timer and free the slot). Holding the lock across `cb()`
/// would also deadlock outright the moment a callback called `once`.
struct Timers {
    entries: [Option<TimerEntry>; MAX_TIMERS],
    next_id: u32,
}

static TIMERS: crate::smp::Spinlock<Timers> = crate::smp::Spinlock::new(Timers {
    entries: [const { None }; MAX_TIMERS],
    next_id: 1,
});

/// Register a one-shot timer. Returns the timer id, or 0 if the table is full.
pub fn once(ms: u32, callback: fn()) -> u32 {
    register(ms, 0, callback)
}

/// Register a periodic timer. Returns the timer id, or 0 if the table is full.
pub fn every(ms: u32, callback: fn()) -> u32 {
    register(ms, ms, callback)
}

fn register(ms: u32, interval: u32, callback: fn()) -> u32 {
    // The tick first, through the lock, before taking the timer table's own
    // critical section: the two must not nest.
    let now = scheduler::with(|s| s.ticks());
    TIMERS.with(|t| {
        let id = t.next_id;
        for slot in t.entries.iter_mut() {
            if slot.is_none() {
                *slot = Some(TimerEntry {
                    id,
                    fire_at: now.wrapping_add(ms as u64),
                    interval,
                    callback: Some(callback),
                });
                t.next_id = id.wrapping_add(1).max(1);
                return id;
            }
        }
        0 // table full — W3.5: surfaced to caller, not silently "successful"
    })
}

/// Cancel a timer by id.
pub fn cancel(id: u32) {
    TIMERS.with(|t| {
        for slot in t.entries.iter_mut() {
            if matches!(slot, Some(e) if e.id == id) {
                *slot = None;
                break;
            }
        }
    });
}

/// Fire any due timers. Called from the trap handler, outside the scheduler
/// lock, taking the timer lock itself for each table access.
///
/// No `&mut` into the static `TIMERS` array is ever held across a callback
/// invocation (item 6). The original code kept `entry`/`slot` borrowed for
/// the callback's entire duration and used them again afterward — but the
/// callback runs arbitrary code, including `once`/`cancel`/`every`, which
/// take their own `&mut` into the same static. That's an aliasing violation
/// on its own, and worse: if the callback frees *its own* slot (`cancel`)
/// and a subsequent `once()` reuses that index for an unrelated timer, the
/// stale post-call `entry`/`slot` reference would then corrupt that
/// unrelated timer's `fire_at`/interval. Instead we snapshot the small bit
/// of Copy data we need, drop the borrow, invoke the callback, then
/// re-acquire the slot by index and verify the id still matches before
/// mutating it.
pub fn process_timers(now: u64) {
    for i in 0..MAX_TIMERS {
        // Snapshot (id, callback, interval) for a due entry, if any, then let
        // the borrow end here — nothing below holds a live reference into
        // `TIMERS` while `cb()` runs.
        let due = TIMERS.with(|t| {
            t.entries[i].as_ref().and_then(|e| {
                if e.fire_at <= now {
                    Some((e.id, e.callback, e.interval))
                } else {
                    None
                }
            })
        });
        let (id, callback, interval) = match due {
            Some(d) => d,
            None => continue,
        };

        if let Some(cb) = callback {
            // Trap-context marker so the callback's own attempts to block
            // (mutex lock, queue send/recv, sleep) refuse instead of
            // wedging the interrupted task (item 11).
            let _guard = crate::interrupt::InterruptGuard::enter();
            cb();
        }

        // Re-acquire by index and verify identity: the callback may have
        // canceled this very timer (freeing slot `i`) and a fresh `once()`/
        // `every()` may have already reused it for something else. Only
        // mutate if slot `i` still holds *this* timer.
        TIMERS.with(|t| {
            let still_same = matches!(&t.entries[i], Some(e) if e.id == id);
            if still_same {
                if interval > 0 {
                    if let Some(entry) = &mut t.entries[i] {
                        entry.fire_at = now.wrapping_add(interval as u64);
                    }
                } else {
                    t.entries[i] = None;
                }
            }
        });
    }
}
