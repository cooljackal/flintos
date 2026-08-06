// SPDX-License-Identifier: Apache-2.0

//! Three tasks at three priorities, each on its own period.
//!
//! This is the application the kernel is verified against: if preemption,
//! priority ordering or timed wakeup break, this is where it shows.
//!
//! Each log line carries the task name, its own priority, and a per-task
//! counter, and the console prefixes every line with the tick. That makes the
//! interesting failures legible straight from the log:
//!
//! - same-priority tasks round-robin against each other
//! - a higher-priority task preempts a lower one
//! - one task hogging the CPU shows up as the *other* counters freezing while
//!   the tick keeps advancing — which is different from the kernel dying, and
//!   looks identical if you only print one counter

#![no_std]
#![no_main]

use api::task;
use hal::types::Priority;

kernel::flint_app!(main);

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

fn main() {
    task::spawn("sensor", task_sensor, SENSOR_PRIORITY, STACK);
    task::spawn("consumer", task_consumer, CONSUMER_PRIORITY, STACK);
    task::spawn("housekeep", task_housekeep, HOUSEKEEP_PRIORITY, STACK);
}

fn task_sensor() {
    let mut n = 0u32;
    loop {
        n += 1;
        api::log_info!("[sensor] prio={:?} n={}", SENSOR_PRIORITY, n);
        task::sleep_ms(500);
    }
}

fn task_consumer() {
    let mut n = 0u32;
    loop {
        n += 1;
        api::log_info!("[consumer] prio={:?} n={}", CONSUMER_PRIORITY, n);
        task::sleep_ms(1000);
    }
}

fn task_housekeep() {
    let mut n = 0u32;
    loop {
        task::sleep_ms(3000);
        n += 1;
        api::log_info!("[housekeep] prio={:?} n={}", HOUSEKEEP_PRIORITY, n);
    }
}
