// SPDX-License-Identifier: Apache-2.0

//! Why the chip last reset.
//!
//! The hardware records this and it survives the reset, which makes it the
//! only direct evidence available after the fact. Everything else — how long
//! the board ran, what the last line on the console was — is inference.
//!
//! This exists because two rounds of reasoning about *which* watchdog was
//! resetting a board produced two wrong answers. The register says.
//!
//! Values from esp-idf's `esp_reset_reason_t` mapping over
//! `RTC_CNTL_RESET_STATE_REG`.

use crate::rtc;

/// `RTC_CNTL_RESET_STATE_REG`. The PRO CPU's cause is bits [5:0].
use rtc::RESET_STATE as RTC_CNTL_RESET_STATE;
const RESET_CAUSE_PROCPU_MASK: u32 = 0x3F;

/// Raw reset cause for the PRO CPU.
///
/// # Safety
/// Reads an RTC register. No side effects.
#[inline]
pub unsafe fn cause() -> u32 {
    (RTC_CNTL_RESET_STATE as *const u32).read_volatile() & RESET_CAUSE_PROCPU_MASK
}

/// A short name for a reset cause.
///
/// The watchdog causes are distinguished from each other deliberately: "a
/// watchdog reset the board" is not an actionable statement when three of them
/// exist and they are armed for different reasons.
pub const fn name(cause: u32) -> &'static str {
    match cause {
        1 => "power-on",
        3 => "software (system)",
        4 => "legacy watchdog",
        5 => "deep sleep",
        6 => "SDIO",
        7 => "TIMG0 watchdog (system)",
        8 => "TIMG1 watchdog (system)",
        9 => "RTC watchdog (system)",
        10 => "intrusion",
        11 => "TIMG watchdog (CPU)",
        12 => "software (CPU)",
        13 => "RTC watchdog (CPU)",
        14 => "external pin",
        15 => "brownout",
        16 => "RTC watchdog (RTC domain)",
        _ => "unknown",
    }
}

/// Whether a cause is one of the watchdogs.
pub const fn is_watchdog(cause: u32) -> bool {
    matches!(cause, 4 | 7 | 8 | 9 | 11 | 13 | 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_register_is_where_esp_idf_reads_it() {
        assert_eq!(RTC_CNTL_RESET_STATE, 0x3FF4_8034);
    }

    #[test]
    fn each_watchdog_is_named_distinctly() {
        // "a watchdog reset the board" is not actionable when three exist and
        // are armed for different reasons -- which is exactly the confusion
        // this module was added to end.
        assert_eq!(name(7), "TIMG0 watchdog (system)");
        assert_eq!(name(8), "TIMG1 watchdog (system)");
        assert_eq!(name(9), "RTC watchdog (system)");
        assert_eq!(name(16), "RTC watchdog (RTC domain)");
        assert_ne!(name(7), name(8));
        assert_ne!(name(8), name(9));
    }

    #[test]
    fn a_normal_boot_is_not_reported_as_a_watchdog() {
        assert!(!is_watchdog(1), "power-on");
        assert!(!is_watchdog(12), "software reset");
        assert!(is_watchdog(9));
        assert!(is_watchdog(16));
    }

    #[test]
    fn the_mask_keeps_only_the_procpu_field() {
        // Bits above 5 belong to the APP CPU's cause. Letting them through
        // would decode one core's reset as the other's.
        assert_eq!(RESET_CAUSE_PROCPU_MASK, 0x3F);
        assert_eq!(0x49 & RESET_CAUSE_PROCPU_MASK, 9, "0x49 is cause 9 plus APP bits");
        assert_eq!(name(0x49 & RESET_CAUSE_PROCPU_MASK), "RTC watchdog (system)");
    }

    #[test]
    fn an_undocumented_cause_is_reported_as_unknown_not_guessed() {
        assert_eq!(name(0), "unknown");
        assert_eq!(name(63), "unknown");
    }
}
