// SPDX-License-Identifier: Apache-2.0

//! Power management: sleep, and reconciling the kernel tick on wake.
//!
//! The chip-level sleep FSM is reached through the [`hal::power::LowPower`]
//! seam, selected with the SoC (`board::SelectedSoc`) exactly as
//! [`crate::clock`] selects its counter; this module is the kernel half — it
//! drives that primitive and, crucially, keeps the scheduler's clock honest
//! across a light sleep, naming no chip to do it.
//!
//! # Why the tick needs reconciling
//!
//! The system tick is driven by the Xtensa CCOUNT/CCOMPARE0 timer, which
//! counts CPU cycles. Light sleep gates the CPU clock, so CCOUNT — and the
//! Timer0 interrupt that increments the tick — both freeze for the whole
//! sleep. On wake the kernel's notion of time is behind reality by exactly the
//! time slept. Left alone, every `sleep_ms`, timeout and timestamp taken after
//! the sleep would be wrong, and a task that slept "until now + 100 ms" would
//! wake far too late.
//!
//! The RTC slow clock keeps running through light sleep, so the SoC primitive
//! measures the real elapsed time from it and hands it back. This module turns
//! that into whole tick periods and adds them — [`ticks_elapsed`] — so the
//! tick catches up. It only ever *adds*: the counter cannot go backwards, no
//! matter what the measurement returns.
//!
//! # Deferred
//!
//! - **Deep sleep** returns nothing to reconcile — it wakes as a reset, so the
//!   tick restarts from zero with the rest of the system. `deep_sleep` is
//!   here for completeness and is exercised by construction, not by the
//!   self-test (it would reset the board mid-run).
//! - **Auto light sleep from the idle task** (the `waiti` hook) is a follow-up;
//!   this cut only offers an explicit call.
//! - **Non-timer wake sources** (RTC-GPIO, touch, ULP) are a follow-up in the
//!   SoC layer.

/// Whole tick periods in `elapsed_us`, for reconciling the tick after a sleep.
///
/// Rounds down: a residue shorter than one period cannot advance a
/// whole-tick counter, and rounding up would let repeated short sleeps run the
/// clock fast. The lost sub-tick remainder is bounded by one period (1 ms on
/// every current board) and does not accumulate across calls, because each
/// sleep measures its own true elapsed time from the RTC rather than trusting
/// the requested duration.
///
/// `period_us` of zero — the tick timer never initialised — yields zero rather
/// than dividing by zero, so a reconciliation before boot completes is a no-op
/// instead of a fault.
pub fn ticks_elapsed(elapsed_us: u64, period_us: u32) -> u64 {
    if period_us == 0 {
        return 0;
    }
    elapsed_us / period_us as u64
}

/// Light sleep for `ms` milliseconds, then reconcile the tick.
///
/// Returns the measured elapsed time in milliseconds (from the RTC, not the
/// requested duration). The scheduler's `now()` is guaranteed not to have gone
/// backwards across the call: [`ticks_elapsed`] only ever advances it.
///
/// This pauses the CPU. Call it from a context where that is safe — the idle
/// task, or an application task that means to block — never from an interrupt.
///
/// # Panics
/// If the SoC sleep primitive reports the RTC clock is stopped or the FSM
/// never resolved: continuing would mean running with a clock the kernel knows
/// is wrong, which the project's philosophy forbids over a plausible-looking
/// wrong result.
#[cfg(all(target_os = "none", feature = "soc-esp32"))]
pub fn light_sleep(ms: u32) -> u64 {
    use crate::arch::Tick;
    use hal::power::LowPower;

    let period_us = crate::board::active::TICK_PERIOD_US;

    // Enter with interrupts masked, as esp-idf disables them around
    // `esp_light_sleep_start`: an ISR taken between "start the FSM" and the CPU
    // halting could context-switch away mid-entry. Masking does not block the
    // wake — the RTC FSM drives the CPU clock directly, independent of the
    // interrupt system, and this path polls the FSM's raw flag rather than
    // taking an RTC interrupt.
    //
    // The SoC's sleep FSM is reached through the `LowPower` seam, so this names
    // no chip; a SoC without one reports `Unsupported` here.
    let elapsed_us = crate::arch::cs_with(|| {
        match unsafe { <crate::board::SelectedSoc as LowPower>::light_sleep((ms as u64) * 1_000) } {
            Ok(us) => us,
            Err(e) => crate::debug::panic::handle(&format_args!("light_sleep failed: {:?}", e)),
        }
    });

    // Forwards only; `now()` cannot regress.
    unsafe { Tick::advance(ticks_elapsed(elapsed_us, period_us)) };
    elapsed_us / 1_000
}

/// Deep sleep for `ms` milliseconds. Does not return on success — the timer
/// wake arrives as a chip reset into the bootloader.
///
/// Reaching the line after this call means the sleep was rejected (a wake was
/// already pending) or the FSM never resolved; both leave the CPU running and
/// nothing to reconcile, since no time was lost.
///
/// # Panics
/// As [`light_sleep`], if the RTC clock is stopped.
#[cfg(all(target_os = "none", feature = "soc-esp32"))]
pub fn deep_sleep(ms: u32) {
    use hal::power::{LowPower, SleepError};
    match unsafe { <crate::board::SelectedSoc as LowPower>::deep_sleep((ms as u64) * 1_000) } {
        // Rejected: CPU still running, no state lost, nothing to do. Unsupported
        // is the same shape on a SoC with no sleep FSM.
        Ok(()) | Err(SleepError::Rejected) | Err(SleepError::Unsupported) => {}
        Err(e) => crate::debug::panic::handle(&format_args!("deep_sleep failed: {:?}", e)),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 ms tick period — every current board's `TICK_PERIOD_US`.
    const PERIOD_US: u32 = 1_000;

    #[test]
    fn elapsed_time_becomes_whole_ticks() {
        // A clean 20 ms sleep at a 1 ms tick is 20 ticks.
        assert_eq!(ticks_elapsed(20_000, PERIOD_US), 20);
        // A 100 ms sleep is 100 ticks.
        assert_eq!(ticks_elapsed(100_000, PERIOD_US), 100);
    }

    #[test]
    fn a_sub_tick_sleep_adds_nothing_rather_than_running_the_clock_fast() {
        // Less than one period: no whole tick elapsed, so add zero. Rounding up
        // here would let a burst of short sleeps advance the clock faster than
        // real time.
        assert_eq!(ticks_elapsed(999, PERIOD_US), 0);
        assert_eq!(ticks_elapsed(0, PERIOD_US), 0);
        // Just over one period rounds down to exactly one.
        assert_eq!(ticks_elapsed(1_500, PERIOD_US), 1);
    }

    #[test]
    fn reconciliation_never_moves_the_clock_backwards() {
        // The property the whole sleep path protects: whatever the measurement
        // says, the number of ticks added is non-negative, so a counter that
        // only ever adds it cannot regress. Modelled here as a running tick.
        let mut tick: u64 = 5_000;
        for elapsed_us in [0u64, 500, 1_000, 20_000, 999, u64::MAX] {
            let before = tick;
            tick = tick.wrapping_add(ticks_elapsed(elapsed_us, PERIOD_US));
            assert!(tick >= before, "tick went backwards on {elapsed_us} us");
        }
    }

    #[test]
    fn an_uninitialised_tick_period_is_a_no_op_not_a_divide_by_zero() {
        assert_eq!(ticks_elapsed(20_000, 0), 0);
    }

    #[test]
    fn measured_time_not_requested_time_drives_the_count() {
        // The RC slow clock is 5–10% off, so a 20 ms *request* may measure as
        // 18–22 ms. Reconciliation follows the measurement, which is what keeps
        // the error from accumulating: each of these lands within a tick or two
        // of the truth, never compounding.
        assert_eq!(ticks_elapsed(18_000, PERIOD_US), 18);
        assert_eq!(ticks_elapsed(22_000, PERIOD_US), 22);
    }
}
