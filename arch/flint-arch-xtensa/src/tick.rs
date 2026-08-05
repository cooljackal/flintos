// SPDX-License-Identifier: Apache-2.0

//! Tick source using the Xtensa CCOUNT / CCOMPARE0 timer (plan W1.1).
//!
//! CCOUNT free-runs at the CPU frequency. A CCOMPARE0 match raises the internal
//! Timer0 interrupt (CPU interrupt 6, level-1). The interrupt is acknowledged
//! and re-armed **only** by writing CCOMPARE0 — there is no separate ack
//! register. This is the single authoritative tick counter for the system.

use core::sync::atomic::{AtomicU32, Ordering};
use flint_hal::tick::TickSource;
use crate::registers;
use crate::critical_section;

pub struct XtensaTick;

/// CPU clock feeding CCOUNT, in Hz.
///
/// **This is an assumption, and every timeout in the system scales with it.**
/// Nothing in Flint's boot path programs the clock tree, so the CPU runs at
/// whatever frequency the second-stage bootloader left it at. espflash writes
/// the stock ESP-IDF bootloader, which configures 80 MHz and then expects the
/// *application* to raise the clock during its own init. Flint has no such
/// init, so 80 MHz is what the part is actually running at.
///
/// The previous value of 240 MHz was the figure an ESP-IDF application would
/// see *after* calling `esp_clk_init()`, which never happens here -- it made
/// every tick period three times shorter than intended.
///
/// If the demo tasks visibly run at the wrong rate, this constant is the first
/// thing to check: the error is exactly the ratio of the real clock to this
/// value. The durable fix is to program the PLL explicitly during boot, or to
/// read the configured frequency back from the clock registers, rather than
/// assuming one. Tracked in doc/review-findings.md.
const CPU_HZ: u64 = 80_000_000;

/// CCOUNT increments per tick period (set in `init`). Fits u32 for any
/// reasonable period (1 ms @ 240 MHz = 240_000).
static TICKS_PER_PERIOD: AtomicU32 = AtomicU32::new(0);

/// The one and only system tick counter. The Xtensa LX6 has no 64-bit atomics,
/// so this is a plain `u64`: written only from `tick()` (trap context, interrupts
/// masked) and read under a critical section in `now()` to avoid torn reads.
static mut TICK_COUNT: u64 = 0;

impl TickSource for XtensaTick {
    fn init(period_us: u32) {
        let per = ((period_us as u64) * CPU_HZ / 1_000_000) as u32;
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
    fn tick() -> bool {
        unsafe {
            let per = TICKS_PER_PERIOD.load(Ordering::Relaxed);
            // Advance from the previous compare to avoid drift; if we have
            // fallen behind by more than one period, catch up to "now".
            let prev = registers::read_ccompare0();
            let mut next = prev.wrapping_add(per);
            let now = registers::read_ccount();
            // If `next` is already in the past, re-base on now + period.
            if next.wrapping_sub(now) > per {
                next = now.wrapping_add(per);
            }
            registers::set_ccompare0(next); // ack + re-arm
            // Safe: only the trap handler (interrupts masked) writes this.
            TICK_COUNT = TICK_COUNT.wrapping_add(1);
        }
        true
    }

    fn now() -> u64 {
        critical_section::with(|| unsafe { *core::ptr::addr_of!(TICK_COUNT) })
    }
}
