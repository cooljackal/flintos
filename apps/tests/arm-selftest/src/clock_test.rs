// SPDX-License-Identifier: Apache-2.0

//! Fresh-nonce, dual-core acceptance of the existing SoC clock contract.
use hal::soc::SystemOnChip;
use kernel::board::SelectedSoc;
use portable_atomic::{AtomicU32, Ordering};

// state, nonce, configured Hz, boot Hz, min Hz, max Hz, core0 count,
// core1 count, busy retries, timer elapsed us, scheduler elapsed ms.
#[no_mangle]
static CLOCK_RESULTS: [AtomicU32; 11] = [const { AtomicU32::new(0) }; 11];
#[no_mangle]
static CLOCK_NONCE: AtomicU32 = AtomicU32::new(0);
const SAMPLES: u32 = 32;

fn wait_for_nonce() -> u32 {
    let started = soc_rp2040::timer_us();
    loop {
        let nonce = CLOCK_NONCE.load(Ordering::Acquire);
        if nonce != 0 {
            return nonce;
        }
        if soc_rp2040::timer_us().wrapping_sub(started) >= 60_000_000 {
            fail(1);
        }
        api::task::sleep_ms(1);
    }
}

fn fail(code: u32) -> ! {
    CLOCK_RESULTS[0].store(0xbad0_0000 | code, Ordering::Release);
    loop {
        api::task::sleep_ms(100);
    }
}

fn sample(core: usize) {
    let started = soc_rp2040::timer_us();
    while CLOCK_RESULTS[6 + core].load(Ordering::Acquire) < SAMPLES {
        if soc_rp2040::timer_us().wrapping_sub(started) >= 2_000_000 {
            fail(2);
        }
        match SelectedSoc::measure_cpu_hz(|| None) {
            Some(hz) => {
                if hz.abs_diff(SelectedSoc::DEFAULT_CPU_HZ) > 5_000 {
                    fail(3);
                }
                CLOCK_RESULTS[4].fetch_min(hz, Ordering::AcqRel);
                CLOCK_RESULTS[5].fetch_max(hz, Ordering::AcqRel);
                CLOCK_RESULTS[6 + core].fetch_add(1, Ordering::Release);
            }
            None => {
                CLOCK_RESULTS[8].fetch_add(1, Ordering::Relaxed);
            }
        }
        api::task::sleep_ms(1);
    }
}

pub fn peer() {
    wait_for_nonce();
    sample(1);
}

pub fn run() {
    CLOCK_RESULTS[2].store(SelectedSoc::DEFAULT_CPU_HZ, Ordering::Relaxed);
    CLOCK_RESULTS[3].store(kernel::arch::Tick::cpu_hz(), Ordering::Relaxed);
    CLOCK_RESULTS[4].store(u32::MAX, Ordering::Relaxed);
    CLOCK_RESULTS[0].store(0x1740_0001, Ordering::Release);
    let nonce = wait_for_nonce();
    CLOCK_RESULTS[1].store(nonce, Ordering::Release);
    sample(0);
    let started = soc_rp2040::timer_us();
    while CLOCK_RESULTS[7].load(Ordering::Acquire) != SAMPLES {
        if soc_rp2040::timer_us().wrapping_sub(started) >= 2_000_000 {
            fail(4);
        }
        api::task::sleep_ms(1);
    }
    let ticks = api::timer::now_ms();
    let micros = soc_rp2040::timer_us();
    api::task::sleep_ms(100);
    let elapsed_us = soc_rp2040::timer_us().wrapping_sub(micros);
    let elapsed_ms = api::timer::now_ms().wrapping_sub(ticks) as u32;
    CLOCK_RESULTS[9].store(elapsed_us, Ordering::Relaxed);
    CLOCK_RESULTS[10].store(elapsed_ms, Ordering::Relaxed);
    if !(90_000..=120_000).contains(&elapsed_us) || !(90..=120).contains(&elapsed_ms) {
        fail(5);
    }
    api::log_info!("[FLINT] CLOCK samples=64 tick-check=pass");
    CLOCK_RESULTS[0].store(0x1740_600d, Ordering::Release);
    loop {
        api::task::sleep_ms(100);
    }
}
