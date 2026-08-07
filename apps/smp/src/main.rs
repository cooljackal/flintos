// SPDX-License-Identifier: Apache-2.0

//! Runs the scheduler on both ESP32 cores and shows where each task ran.
//!
//! Three things have to be true for #20, and each has a task here:
//!
//! - a task pinned to core 1 runs **only** on core 1
//! - a task pinned to core 0 runs **only** on core 0
//! - an unpinned task can run on **either**
//!
//! Each task records which core it observed itself on, every iteration. That is
//! the only evidence that means anything: a pinned task that happens never to
//! be preempted proves nothing, so they all sleep, which forces a switch.

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use api::task;
use hal::types::Priority;
use kernel::smp::Spinlock;

kernel::flint_app!(main, abi = 1);

/// Per-task, per-core sighting counts. `seen[task][core]`.
static SEEN: Spinlock<[[u32; 2]; 3]> = Spinlock::new([[0; 2]; 3]);

const PINNED_0: usize = 0;
const PINNED_1: usize = 1;
const FLOATING: usize = 2;

/// Set once the second core has joined, so the report waits for it.
static SECOND_CORE_UP: AtomicU32 = AtomicU32::new(0);

fn main() {
    // The second core, brought into the scheduler rather than given a private
    // loop. From here it takes traps, ticks and runs tasks like core 0.
    unsafe {
        arch_xtensa::appcpu::prepare(second_core);
        soc_esp32::appcpu::start(arch_xtensa::appcpu::_flint_appcpu_entry);
    }

    task::spawn_on(0, "pin0", pinned_0, Priority::Normal(2), 4096);
    task::spawn_on(1, "pin1", pinned_1, Priority::Normal(2), 4096);
    task::spawn("float", floating, Priority::Normal(3), 4096);
    task::spawn("report", report, Priority::Normal(1), 4096);
}

/// The second core's entry. Never returns — it becomes that core's idle task.
///
/// In IRAM: this runs before the core reaches anything in flash, and the
/// instruction cache is enabled by `appcpu::start` from the other core.
#[link_section = ".iram1.second_core"]
extern "C" fn second_core() -> ! {
    SECOND_CORE_UP.store(1, Ordering::SeqCst);
    unsafe { kernel::boot::join_scheduler() }
}

fn record(which: usize) {
    let core = kernel::smp::current_core().0 as usize;
    SEEN.with(|s| s[which][core] += 1);
}

fn pinned_0() {
    loop {
        record(PINNED_0);
        task::sleep_ms(7);
    }
}

fn pinned_1() {
    loop {
        record(PINNED_1);
        task::sleep_ms(7);
    }
}

fn floating() {
    loop {
        record(FLOATING);
        task::sleep_ms(5);
    }
}

/// Report what actually happened, and judge it.
fn report() {
    task::sleep_ms(300);
    api::log_info!(
        "[smp] second core up: {}",
        SECOND_CORE_UP.load(Ordering::SeqCst) == 1
    );

    for round in 1..=6u32 {
        task::sleep_ms(1000);
        let seen = SEEN.with(|s| *s);

        for (name, i) in [("pin0", PINNED_0), ("pin1", PINNED_1), ("float", FLOATING)] {
            api::log_info!(
                "[smp] {} round {}: core0 {} core1 {}",
                name, round, seen[i][0], seen[i][1]
            );
        }

        // A pinned task on the wrong core is the failure this whole change is
        // about. Report it as an error rather than leaving it to be read out
        // of the numbers.
        if seen[PINNED_0][1] != 0 {
            api::log_error!("[smp] pin0 ran on core 1 {} times", seen[PINNED_0][1]);
        }
        if seen[PINNED_1][0] != 0 {
            api::log_error!("[smp] pin1 ran on core 0 {} times", seen[PINNED_1][0]);
        }
        // And a pinned task that never ran at all is just as wrong, quietly.
        if seen[PINNED_1][1] == 0 {
            api::log_error!("[smp] pin1 never ran on core 1");
        }
        if seen[FLOATING][0] != 0 && seen[FLOATING][1] != 0 {
            api::log_info!("[smp] float ran on both cores");
        }
    }
    loop {
        task::sleep_ms(1000);
    }
}
