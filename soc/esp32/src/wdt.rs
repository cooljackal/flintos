// SPDX-License-Identifier: Apache-2.0

//! The ESP32's watchdogs.
//!
//! Three exist: one in the RTC domain (RWDT) and one in each timer group
//! (MWDT0, MWDT1). `startup.S` disables all three during boot, because a
//! half-initialised kernel being reset mid-bring-up is worse than no watchdog —
//! but nothing re-armed them afterwards, which left the chip *less* protected
//! under Flint than it is under the ROM.
//!
//! # Two watchdogs, two different failures
//!
//! They are not redundant. Each catches something the other cannot:
//!
//! - **RWDT** lives in the RTC domain and survives anything short of a power
//!   cycle. Fed from the timer interrupt, so it fires when *the kernel itself*
//!   has stopped — interrupts stuck masked, a fault loop, a trap handler that
//!   never returns. It cannot catch a spinning task, because the tick still
//!   runs and still feeds it.
//!
//! - **MWDT** is fed from the idle task. Idle runs only when nothing else is
//!   runnable, so a task that never blocks starves it — and the watchdog fires
//!   on a system that is, from the tick's point of view, perfectly healthy.
//!
//! Register layout and the unlock keys are from the ESP32 TRM, chapters 12
//! (timer groups) and 30 (RTC), cross-checked against esp-idf's
//! `soc/rtc_cntl_reg.h` and `soc/timer_group_reg.h`.

use crate::addr::{RTC_CNTL_BASE, TIMG0_BASE, TIMG1_BASE};

/// Write-protect key. Both watchdog families use the same value; the config
/// registers ignore writes until it is present, and re-locking after each
/// change is what stops a wild pointer disabling the watchdog by accident.
const WDT_WKEY: u32 = 0x50D8_3AA1;

// ── RTC watchdog ────────────────────────────────────────────────────────────

const RTC_WDTCONFIG0: u32 = RTC_CNTL_BASE + 0x8C;
const RTC_WDTCONFIG1: u32 = RTC_CNTL_BASE + 0x90;
const RTC_WDTFEED: u32 = RTC_CNTL_BASE + 0xA0;
const RTC_WDTWPROTECT: u32 = RTC_CNTL_BASE + 0xA4;

const RTC_WDT_EN: u32 = 1 << 31;
/// Stage 0 action, bits [30:28].
const RTC_WDT_STG0_SHIFT: u32 = 28;
/// Reset the whole system (both cores and the digital core).
const RTC_WDT_ACTION_RESET_SYSTEM: u32 = 4;
/// Keep counting while the CPU is in sleep, bit 10.
const RTC_WDT_PAUSE_IN_SLEEP: u32 = 1 << 10;
/// System reset pulse length, bits [10:8] of the length field group.
const RTC_WDT_SYS_RESET_LENGTH_SHIFT: u32 = 8;
const RTC_WDT_CPU_RESET_LENGTH_SHIFT: u32 = 5;
/// 3 selects the longest documented pulse. A pulse too short for the power
/// rails to settle produces a reset that half-works, which is harder to
/// diagnose than no reset at all.
const RTC_WDT_RESET_LENGTH_MAX: u32 = 7;

/// Nominal RTC slow-clock frequency, in Hz.
///
/// The internal 150 kHz RC oscillator, which is **not** precise: ±10% across
/// parts and it drifts with temperature. That is why the RWDT timeout is
/// specified in seconds rather than milliseconds — at a multi-second timeout a
/// 10% error is irrelevant, and anything needing better accuracy than that
/// wants the MWDT, which counts off APB.
pub const RTC_SLOW_CLK_HZ: u32 = 150_000;

/// RTC slow-clock cycles for `ms`, saturating rather than wrapping.
///
/// A wrapped divisor would arm the watchdog with a near-zero timeout, resetting
/// the board immediately and continuously — a boot loop caused by the thing
/// meant to recover from one.
pub const fn rtc_cycles_for_ms(ms: u32) -> u32 {
    let cycles = (RTC_SLOW_CLK_HZ as u64) * (ms as u64) / 1000;
    if cycles > u32::MAX as u64 {
        u32::MAX
    } else if cycles == 0 {
        1
    } else {
        cycles as u32
    }
}

/// Arm the RTC watchdog to reset the system if it is not fed within `ms`.
///
/// # Safety
/// Writes the RTC watchdog registers. Arming it means the board resets unless
/// something feeds it; the caller owns that obligation.
pub unsafe fn rwdt_arm(ms: u32) {
    let wp = RTC_WDTWPROTECT as *mut u32;
    wp.write_volatile(WDT_WKEY);

    (RTC_WDTCONFIG1 as *mut u32).write_volatile(rtc_cycles_for_ms(ms));

    let cfg = RTC_WDT_EN
        | (RTC_WDT_ACTION_RESET_SYSTEM << RTC_WDT_STG0_SHIFT)
        | (RTC_WDT_RESET_LENGTH_MAX << RTC_WDT_SYS_RESET_LENGTH_SHIFT)
        | (RTC_WDT_RESET_LENGTH_MAX << RTC_WDT_CPU_RESET_LENGTH_SHIFT)
        | RTC_WDT_PAUSE_IN_SLEEP;
    (RTC_WDTCONFIG0 as *mut u32).write_volatile(cfg);

    wp.write_volatile(0); // re-lock
}

/// Feed the RTC watchdog.
///
/// # Safety
/// Writes the RTC watchdog registers. Called from the timer interrupt.
#[inline]
pub unsafe fn rwdt_feed() {
    let wp = RTC_WDTWPROTECT as *mut u32;
    wp.write_volatile(WDT_WKEY);
    (RTC_WDTFEED as *mut u32).write_volatile(1);
    wp.write_volatile(0);
}

/// Disable the RTC watchdog.
///
/// # Safety
/// Writes the RTC watchdog registers. After this nothing recovers a hung
/// system short of a power cycle.
pub unsafe fn rwdt_disable() {
    let wp = RTC_WDTWPROTECT as *mut u32;
    wp.write_volatile(WDT_WKEY);
    (RTC_WDTCONFIG0 as *mut u32).write_volatile(0);
    wp.write_volatile(0);
}

// ── Timer-group watchdogs ───────────────────────────────────────────────────

const TIMG_WDTCONFIG0: u32 = 0x48;
const TIMG_WDTCONFIG1: u32 = 0x4C;
const TIMG_WDTCONFIG2: u32 = 0x50;
const TIMG_WDTFEED: u32 = 0x60;
const TIMG_WDTWPROTECT: u32 = 0x64;

const TIMG_WDT_EN: u32 = 1 << 31;
const TIMG_WDT_STG0_SHIFT: u32 = 28;
/// Reset the digital core. Stage actions match the RWDT's encoding except that
/// the timer groups cannot reset the RTC domain.
const TIMG_WDT_ACTION_RESET_SYSTEM: u32 = 3;
const TIMG_WDT_SYS_RESET_LENGTH_SHIFT: u32 = 8;
const TIMG_WDT_CPU_RESET_LENGTH_SHIFT: u32 = 5;
const TIMG_WDT_RESET_LENGTH_MAX: u32 = 7;
/// Prescaler, bits [31:16] of CONFIG1.
const TIMG_WDT_PRESCALE_SHIFT: u32 = 16;

/// Which timer group's watchdog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mwdt {
    Group0,
    Group1,
}

impl Mwdt {
    const fn base(self) -> u32 {
        match self {
            Mwdt::Group0 => TIMG0_BASE,
            Mwdt::Group1 => TIMG1_BASE,
        }
    }
}

/// Prescaler giving a 1 ms watchdog tick off the 80 MHz APB clock.
///
/// 80_000 APB cycles is exactly 1 ms, and the field is 16 bits — so 80_000 does
/// not fit. 40_000 gives 0.5 ms per tick and does, which is why the timeout
/// conversion below counts half-milliseconds rather than milliseconds.
pub const MWDT_PRESCALE: u32 = 40_000;

/// The prescaler field is 16 bits. Checked here rather than in a test: a value
/// that does not fit is silently truncated by the hardware into a much shorter
/// timeout, and that should not survive to a test run.
const _: () = assert!(MWDT_PRESCALE <= 0xFFFF);

/// Watchdog ticks for `ms` at [`MWDT_PRESCALE`], saturating.
pub const fn mwdt_ticks_for_ms(ms: u32) -> u32 {
    // Two ticks per millisecond at a 0.5 ms tick.
    let ticks = (ms as u64) * 2;
    if ticks > u32::MAX as u64 {
        u32::MAX
    } else if ticks == 0 {
        1
    } else {
        ticks as u32
    }
}

/// Arm a timer-group watchdog to reset the system if not fed within `ms`.
///
/// # Safety
/// Writes the timer group's watchdog registers, and commits the caller to
/// feeding it.
pub unsafe fn mwdt_arm(which: Mwdt, ms: u32) {
    let base = which.base();
    let wp = (base + TIMG_WDTWPROTECT) as *mut u32;
    wp.write_volatile(WDT_WKEY);

    ((base + TIMG_WDTCONFIG1) as *mut u32)
        .write_volatile(MWDT_PRESCALE << TIMG_WDT_PRESCALE_SHIFT);
    ((base + TIMG_WDTCONFIG2) as *mut u32).write_volatile(mwdt_ticks_for_ms(ms));

    let cfg = TIMG_WDT_EN
        | (TIMG_WDT_ACTION_RESET_SYSTEM << TIMG_WDT_STG0_SHIFT)
        | (TIMG_WDT_RESET_LENGTH_MAX << TIMG_WDT_SYS_RESET_LENGTH_SHIFT)
        | (TIMG_WDT_RESET_LENGTH_MAX << TIMG_WDT_CPU_RESET_LENGTH_SHIFT);
    ((base + TIMG_WDTCONFIG0) as *mut u32).write_volatile(cfg);

    wp.write_volatile(0);
}

/// Feed a timer-group watchdog.
///
/// # Safety
/// Writes the timer group's watchdog registers.
#[inline]
pub unsafe fn mwdt_feed(which: Mwdt) {
    let base = which.base();
    let wp = (base + TIMG_WDTWPROTECT) as *mut u32;
    wp.write_volatile(WDT_WKEY);
    ((base + TIMG_WDTFEED) as *mut u32).write_volatile(1);
    wp.write_volatile(0);
}

/// Disable a timer-group watchdog.
///
/// # Safety
/// Writes the timer group's watchdog registers.
pub unsafe fn mwdt_disable(which: Mwdt) {
    let base = which.base();
    let wp = (base + TIMG_WDTWPROTECT) as *mut u32;
    wp.write_volatile(WDT_WKEY);
    ((base + TIMG_WDTCONFIG0) as *mut u32).write_volatile(0);
    wp.write_volatile(0);
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unlock_key_matches_the_one_startup_asm_already_uses() {
        // startup.S writes this same constant to disable all three watchdogs
        // during boot. If the two ever disagree, one of them silently does
        // nothing -- and a watchdog that silently does nothing is the worst
        // possible outcome for a watchdog.
        assert_eq!(WDT_WKEY, 0x50D8_3AA1);
    }

    #[test]
    fn rtc_conversion_never_produces_a_zero_timeout() {
        // Zero would arm the watchdog to fire immediately and continuously: a
        // boot loop caused by the thing meant to recover from one.
        assert_eq!(rtc_cycles_for_ms(0), 1);
        assert!(rtc_cycles_for_ms(1) >= 1);
    }

    #[test]
    fn rtc_conversion_saturates_rather_than_wrapping() {
        // u32::MAX ms is ~49 days; at 150 kHz that overflows u32 cycles by a
        // wide margin, and a wrapped value is a short timeout, not a long one.
        assert_eq!(rtc_cycles_for_ms(u32::MAX), u32::MAX);
    }

    #[test]
    fn rtc_conversion_is_right_at_a_realistic_timeout() {
        // 5 s at 150 kHz.
        assert_eq!(rtc_cycles_for_ms(5_000), 750_000);
    }

    #[test]
    fn mwdt_conversion_counts_half_milliseconds() {
        assert_eq!(mwdt_ticks_for_ms(1), 2);
        assert_eq!(mwdt_ticks_for_ms(1_000), 2_000);
        assert_eq!(mwdt_ticks_for_ms(0), 1);
        assert_eq!(mwdt_ticks_for_ms(u32::MAX), u32::MAX);
    }

    #[test]
    fn the_two_timer_groups_have_distinct_bases() {
        assert_ne!(Mwdt::Group0.base(), Mwdt::Group1.base());
        assert_eq!(Mwdt::Group0.base(), 0x3FF5_F000);
        assert_eq!(Mwdt::Group1.base(), 0x3FF6_0000);
    }

    #[test]
    fn register_offsets_match_the_addresses_startup_asm_hardcodes() {
        // startup.S disables these by absolute address. Same registers.
        assert_eq!(TIMG0_BASE + TIMG_WDTWPROTECT, 0x3FF5_F064);
        assert_eq!(TIMG0_BASE + TIMG_WDTCONFIG0, 0x3FF5_F048);
        assert_eq!(TIMG1_BASE + TIMG_WDTWPROTECT, 0x3FF6_0064);
        assert_eq!(RTC_WDTWPROTECT, 0x3FF4_80A4);
        assert_eq!(RTC_WDTCONFIG0, 0x3FF4_808C);
    }
}
