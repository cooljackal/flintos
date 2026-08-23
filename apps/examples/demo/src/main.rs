// SPDX-License-Identifier: Apache-2.0

//! Three tasks at three priorities, each on its own period.
//!
//! This is the application the kernel is verified against: if preemption,
//! priority ordering or timed wakeup break, this is where it shows. It lives
//! in `examples/` because it is the second thing to read, but it is also the
//! on-target verification workload — `make test-target` flashes this and
//! judges its log — so a change here is a change to what "the kernel works"
//! means.
//!
//! Each log line carries the task's own priority and a per-task counter, and
//! the console prefixes every line with the tick and the task name. That makes the
//! interesting failures legible straight from the log:
//!
//! - same-priority tasks round-robin against each other
//! - a higher-priority task preempts a lower one
//! - one task hogging the CPU shows up as the *other* counters freezing while
//!   the tick keeps advancing — which is different from the kernel dying, and
//!   looks identical if you only print one counter
//!
//! Next: `blink` — the first peripheral, on an M5Stack Atom.

#![no_std]
#![no_main]

use api::prelude::*;

kernel::flint_app!(main, abi = 1);

// Priorities live here as named constants and are used both to spawn a task and
// in what that task logs about itself, so the number in the log can never drift
// from the number the scheduler actually assigned.
const SENSOR_PRIORITY: Priority = Priority::Normal(1);
const CONSUMER_PRIORITY: Priority = Priority::Normal(5);
const HOUSEKEEP_PRIORITY: Priority = Priority::Background(1);

// 4 KiB rather than 2 KiB. A trap lands on the interrupted task's own stack,
// and with logging on the interrupted call chain can already be several frames
// deep (log_info! -> formatting -> the UART driver) before the trap frame and
// _flint_trap's own frame go on top. The stack pool is 96 KiB and three tasks
// use 12 KiB of it, so the headroom is free.
const STACK: usize = 4096;

/// Deliberately hang so the RTC watchdog has to reset the board.
///
/// Masks interrupts and never returns, which is exactly the failure the RTC
/// watchdog exists for: the tick stops, so nothing feeds it. A board that does
/// **not** reset within a few seconds means the watchdog is not really armed.
#[cfg(feature = "watchdog-test-kernel")]
fn hang_with_interrupts_masked() {
    sleep_ms(3_000); // let a few normal log lines out first
    log_info!("masking interrupts and hanging -- expect a reset in ~5 s");
    kernel::arch::cs_with(|| loop {
        core::hint::spin_loop();
    });
}

/// Deliberately spin *without* masking, so only the idle-fed watchdog notices.
///
/// The tick keeps running and keeps feeding the RTC watchdog, so from its point
/// of view the system is perfectly healthy. Idle never runs again, which is the
/// only observable difference -- and the timer-group watchdog is the only thing
/// watching for it.
#[cfg(feature = "watchdog-test-idle")]
fn spin_without_yielding() {
    sleep_ms(3_000);
    log_info!("spinning without yielding -- expect a reset in ~10 s");
    loop {
        core::hint::spin_loop();
    }
}

fn main() {
    // Opt in to the watchdogs. Off unless an application asks, because a board
    // that resets itself for reasons its author never requested is a very
    // confusing first experience -- and on a board being single-stepped, a
    // halted CPU looks exactly like a hung one.
    //
    // Two of them, catching different failures: the RTC watchdog is fed from
    // the timer interrupt and fires if the kernel stops servicing it, while the
    // timer-group one is fed from the idle task and fires if some task stops
    // yielding. A spinning task keeps the tick alive, so only the second
    // notices it.
    unsafe { kernel::watchdog::arm() };

    // Watchdog verification, off unless asked for. Highest normal priority so
    // it is unambiguously the thing holding the system up.
    #[cfg(feature = "watchdog-test-kernel")]
    Task::new("wdt-hang", hang_with_interrupts_masked)
        .priority(Priority::Normal(0))
        .stack(STACK)
        .spawn()
        .expect("spawn wdt-hang");
    #[cfg(feature = "watchdog-test-idle")]
    Task::new("wdt-spin", spin_without_yielding)
        .priority(Priority::Normal(0))
        .stack(STACK)
        .spawn()
        .expect("spawn wdt-spin");

    Task::new("sensor", task_sensor)
        .priority(SENSOR_PRIORITY)
        .stack(STACK)
        .spawn()
        .expect("spawn sensor");
    Task::new("consumer", task_consumer)
        .priority(CONSUMER_PRIORITY)
        .stack(STACK)
        .spawn()
        .expect("spawn consumer");
    Task::new("housekeep", task_housekeep)
        .priority(HOUSEKEEP_PRIORITY)
        .stack(STACK)
        .spawn()
        .expect("spawn housekeep");
}

fn task_sensor() {
    let mut n = 0u32;
    loop {
        n += 1;
        log_info!("prio={SENSOR_PRIORITY:?} n={n}");
        sleep_ms(500);
    }
}

fn task_consumer() {
    let mut n = 0u32;
    loop {
        n += 1;
        log_info!("prio={CONSUMER_PRIORITY:?} n={n}");
        sleep_ms(1000);
    }
}

fn task_housekeep() {
    let mut n = 0u32;
    loop {
        sleep_ms(3000);
        n += 1;
        log_info!("prio={HOUSEKEEP_PRIORITY:?} n={n}");
    }
}
