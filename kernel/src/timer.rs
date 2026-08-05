//! Timer subsystem: sleep and one-shot/periodic callbacks (plan W1, W3.5).
//!
//! `sleep_ms` blocks the current task until the target tick, then requests a
//! switch via the software interrupt. `once`/`every` register software timers
//! whose callbacks fire from the trap handler. Software-timer state is guarded
//! by a critical section against the trap handler.

use flint_arch_xtensa::cs_with;
use crate::scheduler::{self, TaskState};

/// Sleep the current task for `ms` milliseconds.
pub fn sleep_ms(ms: u32) {
    scheduler::with(|sched| {
        let cur = sched.current;
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

static mut TIMERS: [Option<TimerEntry>; MAX_TIMERS] = [const { None }; MAX_TIMERS];
static mut NEXT_TIMER_ID: u32 = 1;

/// Register a one-shot timer. Returns the timer id, or 0 if the table is full.
pub fn once(ms: u32, callback: fn()) -> u32 {
    register(ms, 0, callback)
}

/// Register a periodic timer. Returns the timer id, or 0 if the table is full.
pub fn every(ms: u32, callback: fn()) -> u32 {
    register(ms, ms, callback)
}

fn register(ms: u32, interval: u32, callback: fn()) -> u32 {
    cs_with(|| unsafe {
        let now = scheduler::global().ticks();
        let timers = &mut *core::ptr::addr_of_mut!(TIMERS);
        for slot in timers.iter_mut() {
            if slot.is_none() {
                let id = NEXT_TIMER_ID;
                NEXT_TIMER_ID = NEXT_TIMER_ID.wrapping_add(1).max(1);
                *slot = Some(TimerEntry {
                    id,
                    fire_at: now.wrapping_add(ms as u64),
                    interval,
                    callback: Some(callback),
                });
                return id;
            }
        }
        0 // table full — W3.5: surfaced to caller, not silently "successful"
    })
}

/// Cancel a timer by id.
pub fn cancel(id: u32) {
    cs_with(|| unsafe {
        let timers = &mut *core::ptr::addr_of_mut!(TIMERS);
        for slot in timers.iter_mut() {
            if matches!(slot, Some(e) if e.id == id) {
                *slot = None;
                break;
            }
        }
    });
}

/// Fire any due timers. Called from the trap handler (interrupts already
/// masked), so no extra critical section is taken.
pub fn process_timers(now: u64) {
    unsafe {
        let timers = &mut *core::ptr::addr_of_mut!(TIMERS);
        for slot in timers.iter_mut() {
            if let Some(entry) = slot {
                if entry.fire_at <= now {
                    if let Some(cb) = entry.callback {
                        cb();
                    }
                    if entry.interval > 0 {
                        entry.fire_at = now.wrapping_add(entry.interval as u64);
                    } else {
                        *slot = None;
                    }
                }
            }
        }
    }
}
