// SPDX-License-Identifier: Apache-2.0

//! Tick source using the Xtensa CCOUNT / CCOMPARE0 timer (plan W1.1).
//!
//! CCOUNT free-runs at the CPU frequency. A CCOMPARE0 match raises the internal
//! Timer0 interrupt (CPU interrupt 6, level-1). The interrupt is acknowledged
//! and re-armed **only** by writing CCOMPARE0 — there is no separate ack
//! register. This is the single authoritative tick counter for the system.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use hal::tick::TickSource;
use crate::registers;
use crate::critical_section;

pub struct XtensaTick;

/// CPU clock feeding CCOUNT, in Hz -- **fallback only**.
///
/// Nothing in Flint's boot path programs the clock tree, so historically this
/// constant was a guess at whatever frequency the second-stage bootloader left
/// the part at (issue #6). `init` now *measures* the real frequency at boot by
/// timing CCOUNT against the RTC slow clock (see `measure_cpu_hz` below) and
/// only falls back to this constant if that measurement fails or comes back
/// implausible. If you see the "ASSUMED" line in the boot banner instead of
/// "measured", every timeout in the system is scaled by whatever this
/// constant says, which may not match reality.
///
/// 80 MHz is used here because that's what the stock ESP-IDF 2nd-stage
/// bootloader (which espflash writes) configures before handing off; it
/// expects the *application* to raise the clock during its own init, which
/// Flint does not do. The previous value of 240 MHz was the figure an
/// ESP-IDF application would see only *after* calling `esp_clk_init()`.
const CPU_HZ: u64 = 80_000_000;

/// Nominal RTC slow-clock rate used to measure the CPU frequency. The ESP32
/// defaults `RTC_SLOW_CLK` to the internal 150 kHz RC oscillator (untrimmed,
/// commonly ±(5-10)% off) unless something has switched it to the external
/// 32 kHz crystal or the 8 MHz/256 divider -- Flint's boot path does neither.
/// The oscillator's imprecision is fine here: the only question being asked
/// is "80, 160, or 240 MHz?", and a clock good to 10% resolves that easily.
const RTC_SLOW_HZ_NOMINAL: u64 = 150_000;

/// RTC slow-clock ticks to time the measurement window across. ~1500 ticks is
/// ~10 ms at the nominal 150 kHz rate: long enough to average out RC jitter,
/// short enough not to visibly delay boot.
const MEASURE_RTC_TICKS: u64 = 1500;

/// Bound on CCOUNT cycles to wait for the RTC counter to advance at all, so a
/// missing or stuck RTC block can't hang boot forever. 50,000,000 cycles is
/// well under a second even at the slowest plausible CPU clock (80 MHz ->
/// ~0.6 s), and nowhere near the ~4.29e9-cycle point where a 32-bit CCOUNT
/// read could wrap during the wait.
const MEASURE_TIMEOUT_CYCLES: u32 = 50_000_000;

/// ESP32 CPU frequencies the second-stage bootloader could plausibly have
/// left the part running at.
const PLAUSIBLE_HZ: [u64; 3] = [80_000_000, 160_000_000, 240_000_000];

/// Measured (or assumed) CPU frequency in Hz, in effect since `init` ran.
static CPU_HZ_ACTUAL: AtomicU32 = AtomicU32::new(CPU_HZ as u32);

/// Whether `CPU_HZ_ACTUAL` came from a real RTC measurement (`true`) or the
/// `CPU_HZ` fallback constant because measurement failed/was implausible
/// (`false`). Read by the boot banner to print "measured" vs. "ASSUMED".
static CPU_HZ_MEASURED: AtomicBool = AtomicBool::new(false);

/// CCOUNT increments per tick period (set in `init`). Fits u32 for any
/// reasonable period (1 ms @ 240 MHz = 240_000).
static TICKS_PER_PERIOD: AtomicU32 = AtomicU32::new(0);

/// Time CCOUNT against the RTC slow-clock counter to derive the actual CPU
/// frequency, rounded to the nearest plausible ESP32 clock (80/160/240 MHz).
///
/// Returns `None` if the RTC counter never advanced within the timeout, or if
/// the raw measurement doesn't land close enough to a plausible frequency to
/// trust (more than 25% off the nearest candidate) -- callers must fall back
/// to the documented constant and say so, per issue #6's postmortem: silently
/// using a wrong clock is what caused the bug in the first place.
fn measure_cpu_hz() -> Option<u32> {
    unsafe {
        let rtc0 = registers::rtc_cntl::read_counter(10_000)?;
        let c0 = registers::read_ccount();

        loop {
            let rtc_now = registers::rtc_cntl::read_counter(10_000)?;
            let elapsed_rtc = rtc_now.wrapping_sub(rtc0);
            if elapsed_rtc >= MEASURE_RTC_TICKS {
                let c1 = registers::read_ccount();
                let cycles = c1.wrapping_sub(c0) as u64;
                let raw_hz = cycles * RTC_SLOW_HZ_NOMINAL / elapsed_rtc;
                return round_to_plausible(raw_hz);
            }
            let c_now = registers::read_ccount();
            if c_now.wrapping_sub(c0) > MEASURE_TIMEOUT_CYCLES {
                return None; // RTC counter never moved (or moved too slowly)
            }
        }
    }
}

/// Snap a raw frequency measurement to the nearest plausible ESP32 CPU clock,
/// or `None` if it isn't close to any of them (more than 25% away).
fn round_to_plausible(raw_hz: u64) -> Option<u32> {
    let mut best = PLAUSIBLE_HZ[0];
    let mut best_diff = raw_hz.abs_diff(best);
    for &candidate in &PLAUSIBLE_HZ[1..] {
        let diff = raw_hz.abs_diff(candidate);
        if diff < best_diff {
            best_diff = diff;
            best = candidate;
        }
    }
    if best_diff * 4 > best {
        None
    } else {
        Some(best as u32)
    }
}

/// Re-arm the calling core's CCOMPARE0.
///
/// # Safety
/// Writes this core's timer registers.
unsafe fn rearm_this_core_inner() {
    let per = TICKS_PER_PERIOD.load(Ordering::Relaxed);
    // Advance from the previous compare to avoid drift; if we have fallen
    // behind by more than one period, catch up to "now".
    let prev = registers::read_ccompare0();
    let mut next = prev.wrapping_add(per);
    let now = registers::read_ccount();
    if next.wrapping_sub(now) > per {
        next = now.wrapping_add(per);
    }
    registers::set_ccompare0(next); // ack + re-arm
}

/// The one and only system tick counter. The Xtensa LX6 has no 64-bit atomics,
/// so this is a plain `u64`: written only from `tick()` (trap context, interrupts
/// masked) and read under a critical section in `now()` to avoid torn reads.
static mut TICK_COUNT: u64 = 0;

impl XtensaTick {
    /// The CPU frequency actually in effect (measured, or the fallback
    /// constant if measurement failed). Valid only after `init` has run.
    pub fn cpu_hz() -> u32 {
        CPU_HZ_ACTUAL.load(Ordering::Relaxed)
    }

    /// Whether `cpu_hz()` came from a real RTC measurement rather than the
    /// hardcoded fallback. Valid only after `init` has run.
    pub fn freq_measured() -> bool {
        CPU_HZ_MEASURED.load(Ordering::Relaxed)
    }

    /// CCOUNT ticks per tick-timer period, as programmed by `init`.
    pub fn ticks_per_period() -> u32 {
        TICKS_PER_PERIOD.load(Ordering::Relaxed)
    }
}

impl XtensaTick {
    /// Arm this core's own CCOMPARE0 and unmask its Timer0 interrupt.
    ///
    /// Every core needs its own preemption interrupt — CCOUNT, CCOMPARE0 and
    /// INTENABLE are all per-core — but only one core may own the *time base*.
    /// [`TickSource::init`] does both; this does only the per-core half, for a
    /// core that joins after the clock has already been measured.
    ///
    /// # Safety
    /// Writes this core's timer registers and unmasks an interrupt on it. The
    /// caller must have a trap handler installed, or the first tick is fatal.
    pub unsafe fn init_this_core() {
        let per = TICKS_PER_PERIOD.load(Ordering::Relaxed);
        let ccount = registers::read_ccount();
        registers::set_ccompare0(ccount.wrapping_add(per));
        registers::enable_interrupt(registers::INT_TIMER0);
    }

    /// Re-arm this core's timer without touching the shared tick count.
    ///
    /// The counter is the system's single notion of time. A second core
    /// advancing it too would make every sleep and timeout expire at twice the
    /// rate — silently, and only when the second core happened to be running.
    ///
    /// # Safety
    /// Writes this core's timer registers.
    pub unsafe fn rearm_this_core() {
        rearm_this_core_inner();
    }
}

impl TickSource for XtensaTick {
    fn init(period_us: u32) {
        let (hz, measured) = match measure_cpu_hz() {
            Some(hz) => (hz, true),
            None => (CPU_HZ as u32, false),
        };
        CPU_HZ_ACTUAL.store(hz, Ordering::Relaxed);
        CPU_HZ_MEASURED.store(measured, Ordering::Relaxed);

        let per = ((period_us as u64) * (hz as u64) / 1_000_000) as u32;
        TICKS_PER_PERIOD.store(per, Ordering::Relaxed);
        unsafe {
            let ccount = registers::read_ccount();
            registers::set_ccompare0(ccount.wrapping_add(per));
            // Enable the internal Timer0 interrupt (level-1).
            registers::enable_interrupt(registers::INT_TIMER0);
        }
    }

    /// Called from the trap handler when the Timer0 interrupt is pending.
    /// Re-arms (which clears the interrupt) and advances the counter.
    /// Called from the trap handler when the Timer0 interrupt is pending.
    ///
    /// Re-arms this core's timer -- which is what clears the interrupt -- and
    /// advances the shared counter **only on the boot core**. Every core gets
    /// its own timer interrupt for preemption; exactly one owns time.
    fn tick() -> bool {
        unsafe {
            rearm_this_core_inner();
            if <crate::smp::XtensaSmp as hal::smp::MultiCore>::current_core().is_boot() {
                // Safe: only the boot core's trap handler writes this, with
                // interrupts masked on that core.
                TICK_COUNT = TICK_COUNT.wrapping_add(1);
            }
        }
        true
    }

    fn now() -> u64 {
        critical_section::with(|| unsafe { *core::ptr::addr_of!(TICK_COUNT) })
    }
}
