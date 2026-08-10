// SPDX-License-Identifier: Apache-2.0

//! Tick source using the Xtensa CCOUNT / CCOMPARE0 timer (plan W1.1).
//!
//! CCOUNT free-runs at the CPU frequency. A CCOMPARE0 match raises the internal
//! Timer0 interrupt (CPU interrupt 6, level-1). The interrupt is acknowledged
//! and re-armed **only** by writing CCOMPARE0 — there is no separate ack
//! register. This is the single authoritative tick counter for the system.

use core::sync::atomic::{AtomicU32, Ordering};
use hal::tick::TickSource;
use crate::registers;
use crate::critical_section;

pub struct XtensaTick;

/// Measured (or assumed) CPU frequency in Hz, in effect since `init` ran.
static CPU_HZ_ACTUAL: AtomicU32 = AtomicU32::new(0);

/// CCOUNT increments per tick period (set in `init`). Fits u32 for any
/// reasonable period (1 ms @ 240 MHz = 240_000).
static TICKS_PER_PERIOD: AtomicU32 = AtomicU32::new(0);

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
    /// The CPU frequency the timer was programmed against, as `init` was
    /// told it. Zero before `init` has run.
    pub fn cpu_hz() -> u32 {
        CPU_HZ_ACTUAL.load(Ordering::Relaxed)
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
    fn init(period_us: u32, cpu_hz: u32) {
        let hz = cpu_hz;
        CPU_HZ_ACTUAL.store(hz, Ordering::Relaxed);

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
