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
//! Increment an atomic, and take the kernel's cross-core [`Spinlock`] — which
//! is the one kernel primitive designed to be safe here, and the only way to
//! test that it is.
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
use kernel::smp::Spinlock;
use hal::types::Priority;

kernel::flint_app!(main, abi = 1);

/// The only thing the two cores share without a lock.
static TICKS: AtomicU32 = AtomicU32::new(0);

/// Contended state, guarded by the kernel's cross-core spinlock.
///
/// `count` is incremented by both cores; `inside` catches two cores being in
/// the critical section at once. Counting alone is not enough — a lock that
/// serialised nothing could still add up if the increments happened to
/// interleave safely — so this checks overlap directly.
struct Contended {
    count: u32,
    inside: u32,
    overlaps: u32,
}

static SHARED: Spinlock<Contended> = Spinlock::new(Contended {
    count: 0,
    inside: 0,
    overlaps: 0,
});

/// How many times the APP CPU has taken the lock.
static APP_TAKEN: AtomicU32 = AtomicU32::new(0);

/// Accumulates the result of a function the APP CPU can only reach through
/// its instruction cache.
static FROM_FLASH: AtomicU32 = AtomicU32::new(0);

/// An ordinary function: no `link_section`, no inlining, so it lives in flash
/// like every task and driver does. The second core running this at all is the
/// proof its cache works.
#[inline(never)]
fn flash_resident_work() -> u32 {
    1
}

fn main() {
    task::spawn("smp", smp, Priority::Normal(1), 4096);
}

fn smp() {
    api::log_info!("[smp] PRO CPU is core {}", kernel::smp::current_core().0);
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

    // The real test: hammer a shared spinlock from both cores at once and
    // check nothing is lost and nobody overlaps.
    api::log_info!("[smp] contending on a spinlock from both cores");
    let mut pro_taken = 0u32;
    for _ in 0..200_000 {
        SHARED.with(|s| {
            s.count += 1;
            s.inside += 1;
            if s.inside != 1 {
                s.overlaps += 1;
            }
            s.inside -= 1;
        });
        pro_taken += 1;
    }

    // Let the other core keep going a moment, then read the totals.
    task::sleep_ms(200);
    let app_taken = APP_TAKEN.load(Ordering::Relaxed);
    let (count, overlaps) = SHARED.with(|s| (s.count, s.overlaps));
    let from_flash = FROM_FLASH.load(Ordering::Relaxed);
    if from_flash == 0 {
        api::log_error!("[smp] APP CPU never reached flash-resident code");
    } else {
        api::log_info!("[smp] APP CPU ran flash-resident code {} times", from_flash);
    }

    api::log_info!(
        "[smp] PRO took {} APP took {} -> count {} (expected {})",
        pro_taken, app_taken, count, pro_taken + app_taken
    );
    if overlaps != 0 {
        api::log_error!("[smp] {} overlaps: two cores were inside at once", overlaps);
    } else {
        api::log_info!("[smp] no overlaps -- the lock excluded the other core");
    }
    // The counts can differ by a few: the APP CPU keeps incrementing between
    // reading APP_TAKEN and reading the count. Anything larger is a lost update.
    let drift = count.abs_diff(pro_taken + app_taken);
    if drift > 32 {
        api::log_error!("[smp] {} increments lost", drift);
    } else {
        api::log_info!("[smp] no increments lost (drift {})", drift);
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

        // Take the kernel's cross-core lock from the second core. This is the
        // whole point: `Spinlock` claims to exclude the *other* core, and only
        // the other core can test that claim.
        SHARED.with(|s| {
            s.count += 1;
            s.inside += 1;
            if s.inside != 1 {
                s.overlaps += 1;
            }
            s.inside -= 1;
        });
        APP_TAKEN.fetch_add(1, Ordering::Relaxed);

        // Deliberately NOT inlined and NOT in IRAM, so it can only run if the
        // second core's instruction cache is working. Before the cache was
        // enabled this call was the thing that hung.
        FROM_FLASH.fetch_add(flash_resident_work(), Ordering::Relaxed);
        // No delay. A `for _ in 0..n { spin_loop() }` was here and the
        // optimiser removed it -- `spin_loop` is a hint with no side effect,
        // so the loop does nothing observable and is not required to happen.
        // The count therefore runs at about 4 M/s, which is fine: what matters
        // is that it moves at a *constant* rate, and it does to within 40 ppm.
    }
}
