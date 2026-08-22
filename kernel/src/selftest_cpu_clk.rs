// SPDX-License-Identifier: Apache-2.0

//! CPU-clock self-tests. Included by [`crate::selftest`].
//!
//! The boot path raises the CPU to 240 MHz (`soc_esp32::cpu_clk::set_240mhz`)
//! and then *measures* the clock against the RTC slow oscillator. These tests
//! check that the raise actually took and that the two clock domains agree:
//!
//! 1. The measured CPU frequency is 240 MHz. If `set_240mhz` did nothing, or
//!    left a bad PLL/divider, the measurement snaps to 80 or 160 MHz instead.
//! 2. A CPU-cycle interval (CCOUNT) matches a TIMG-timed interval. TIMG runs
//!    off the APB clock (a fixed 80 MHz whatever the CPU does), so timing a
//!    known number of CPU cycles against it cross-checks the CPU/APB ratio —
//!    the one thing a CPU counting against itself can never confirm. A CPU
//!    still at 80 MHz spends a third of the cycles in the same wall-clock time
//!    and lands far outside the window.
//!
//! Like the TIMG tests, these run from boot context and busy-wait rather than
//! sleeping.

use super::Check;

/// The frequency `set_240mhz` targets. The measurement in `boot.rs` snaps to
/// the nearest plausible clock, so a healthy raise reports exactly this.
#[cfg(target_os = "none")]
const EXPECTED_CPU_HZ: u32 = 240_000_000;

/// The measured CPU clock must be 240 MHz.
///
/// `boot.rs` measured CCOUNT against the RTC slow clock and handed the snapped
/// result to `Tick::init`; this reads it back. Dull on purpose: the interesting
/// failure (a raise that silently didn't happen) shows up as 80 MHz here.
#[cfg(target_os = "none")]
pub(crate) fn cpu_runs_at_240mhz() -> Check {
    use crate::arch::Tick;

    match Tick::cpu_hz() {
        EXPECTED_CPU_HZ => Ok(()),
        0 => Err("cpu_hz is zero — Tick::init did not run before the self-test"),
        80_000_000 => Err("still 80 MHz — the 240 MHz raise did not take"),
        _ => Err("measured CPU clock is not 240 MHz"),
    }
}

/// CPU cycles to time: 2_400_000 is 10 ms at 240 MHz, 30 ms at 80 MHz — far
/// enough apart that no tolerance confuses the two. Deliberately long: the TIMG
/// setup and first read cost a fixed ~0.2 ms, which is ~2% of a 10 ms interval
/// but would be ~18% of a 1 ms one and blow the tolerance for a clock that is
/// in fact correct.
#[cfg(target_os = "none")]
const SPIN_CYCLES: u32 = 2_400_000;

/// The CPU and APB clocks must agree on how long an interval is.
///
/// Spin a fixed number of CPU cycles and time it with a 1 MHz TIMG (off APB).
/// The expected microseconds are `SPIN_CYCLES / (cpu_hz / 1e6)`, computed from
/// the *measured* CPU clock so the two domains cross-check rather than each
/// trusting itself. A wrong CPU/APB ratio — the symptom of a botched raise —
/// misses by a factor, not a few percent.
#[cfg(target_os = "none")]
pub(crate) fn cpu_and_apb_agree_on_an_interval() -> Check {
    use crate::arch::Tick;
    use esp32_timg::{Group, Timer, Timg};

    let cpu_hz = Tick::cpu_hz();
    if cpu_hz == 0 {
        return Err("cpu_hz is zero — cannot compute an expected interval");
    }

    let t = match unsafe { Timg::new(Group::Timg0, Timer::T0, 1_000_000) } {
        Ok(t) => t,
        Err(_) => return Err("could not configure TIMG0 T0 for 1 MHz"),
    };
    unsafe { t.start_free_running() };

    let t0 = unsafe { t.now() };
    super::spin_cycles(SPIN_CYCLES);
    let elapsed_us = unsafe { t.now() }.saturating_sub(t0);
    unsafe { t.stop() };

    if elapsed_us == 0 {
        return Err("TIMG counter never advanced");
    }

    // Expected µs = cycles / (cpu_hz per µs). At 240 MHz, 240_000 cycles → 1000.
    let expected = SPIN_CYCLES as u64 / (cpu_hz as u64 / 1_000_000);
    let (lo, hi) = (expected * 9 / 10, expected * 11 / 10);
    if elapsed_us < lo || elapsed_us > hi {
        return Err("CPU and APB clocks disagree on the interval length");
    }
    Ok(())
}

// Host stand-ins: there is no CCOUNT, no RTC measurement, and no TIMG block.
#[cfg(not(target_os = "none"))]
pub(crate) fn cpu_runs_at_240mhz() -> Check {
    Ok(())
}
#[cfg(not(target_os = "none"))]
pub(crate) fn cpu_and_apb_agree_on_an_interval() -> Check {
    Ok(())
}
