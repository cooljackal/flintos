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
/// **The lock is not held across a callback.** `process_timers` snapshots all
/// due callbacks and advances or removes their entries in one pass, then
/// releases the lock before invoking any of them. Holding the lock across
/// `cb()` would deadlock outright the moment a callback called `once`.
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
/// invocation (item 6). A callback can therefore call `once`, `cancel`, or
/// `every` without aliasing the table or re-entering its lock.
pub fn process_timers(now: u64) {
    // One global critical section per tick, not one per table slot. On
    // RP2040 every critical section takes a chip-wide hardware spinlock; the
    // old 16-acquisition scan let core 0's SysTick repeatedly beat core 1 to
    // that lock and starve ordinary scheduler/mutex work.
    let mut callbacks = [None; MAX_TIMERS];
    TIMERS.with(|t| {
        for (i, slot) in t.entries.iter_mut().enumerate() {
            let Some(entry) = slot.as_mut() else { continue };
            if entry.fire_at > now {
                continue;
            }
            callbacks[i] = entry.callback;
            if entry.interval > 0 {
                entry.fire_at = now.wrapping_add(entry.interval as u64);
            } else {
                *slot = None;
            }
        }
    });

    for cb in callbacks.into_iter().flatten() {
        // Trap-context marker so the callback's own attempts to block
        // (mutex lock, queue send/recv, sleep) refuse instead of
        // wedging the interrupted task (item 11).
        let _guard = crate::interrupt::InterruptGuard::enter();
        cb();
    }
}
