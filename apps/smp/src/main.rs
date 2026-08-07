// SPDX-License-Identifier: Apache-2.0

//! Starts the ESP32's second core and shows it running.
//!
//! The APP CPU is held by three separate mechanisms at reset — RTC stall,
//! clock gate, and reset itself — in two different register domains. Miss one
//! and the core sits silent with nothing reporting a fault, which is why this
//! app exists: a counter incrementing in shared memory is the only proof that
//! survives being wrong about any of it.
//!
//! # What the second core is allowed to do here
//!
//! Increment an atomic. That is all, and the restraint is the point.
//!
//! Flint's scheduler is single-core: `scheduler::global()` hands out `&mut` to
//! a `static`, and a critical section masks interrupts on the calling core
//! only. Running kernel code on both cores is a data race in the language's
//! own terms. So the APP CPU here touches no kernel function, takes no lock,
//! and sends on no queue — it shares exactly one `AtomicU32` with the PRO CPU
//! and nothing else.
//!
//! Making the kernel safe for two cores is the rest of #19, and it is a much
//! larger job than starting a core.

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use api::task;
use hal::types::Priority;

kernel::flint_app!(main, abi = 1);

/// The only thing the two cores share.
static TICKS: AtomicU32 = AtomicU32::new(0);

fn main() {
    task::spawn("smp", smp, Priority::Normal(1), 4096);
}

fn smp() {
    api::log_info!("[smp] APP CPU stalled before start: {}", unsafe {
        soc_esp32::appcpu::is_stalled()
    });

    unsafe {
        // The stack pointer and entry function the trampoline will use, before
        // the core is released to run it.
        arch_xtensa::appcpu::prepare(app_cpu_main);
        soc_esp32::appcpu::start(arch_xtensa::appcpu::_flint_appcpu_entry);
    }

    api::log_info!("[smp] APP CPU stalled after start: {}", unsafe {
        soc_esp32::appcpu::is_stalled()
    });

    // A counter that never moves means the core never started. One that moves
    // means it is fetching, executing and reaching shared memory.
    let mut last = 0u32;
    for i in 1..=12u32 {
        task::sleep_ms(500);
        let now = TICKS.load(Ordering::Relaxed);
        let delta = now.wrapping_sub(last);
        if delta == 0 {
            api::log_error!("[smp] {} APP CPU counter stuck at {}", i, now);
        } else {
            api::log_info!("[smp] {} APP CPU counter {} (+{})", i, now, delta);
        }
        last = now;
    }

    unsafe { soc_esp32::appcpu::stop() };
    let stopped_at = TICKS.load(Ordering::Relaxed);
    task::sleep_ms(500);
    let after = TICKS.load(Ordering::Relaxed);
    if after == stopped_at {
        api::log_info!("[smp] stopped cleanly, counter frozen at {}", after);
    } else {
        api::log_error!("[smp] APP CPU still running after stop: {} -> {}", stopped_at, after);
    }

    loop {
        task::sleep_ms(1000);
    }
}

/// Runs on the APP CPU. Never returns.
///
/// In IRAM, and it has to be: the APP CPU starts with no instruction cache and
/// cannot fetch from flash. Left in `.text` this faults on its first
/// instruction, on a core with no vector table to say so — the counter simply
/// never moves.
///
/// Deliberately trivial: no kernel calls, no logging (the console driver is
/// not safe to share), no allocation. Just an atomic and a delay loop.
#[link_section = ".iram1.app_cpu_main"]
extern "C" fn app_cpu_main() -> ! {
    loop {
        TICKS.fetch_add(1, Ordering::Relaxed);
        // No delay. A `for _ in 0..n { spin_loop() }` was here and the
        // optimiser removed it -- `spin_loop` is a hint with no side effect,
        // so the loop does nothing observable and is not required to happen.
        // The count therefore runs at about 4 M/s, which is fine: what matters
        // is that it moves at a *constant* rate, and it does to within 40 ppm.
    }
}
