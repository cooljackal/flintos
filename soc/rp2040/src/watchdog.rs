// SPDX-License-Identifier: Apache-2.0

//! RP2040 watchdog control and retained reset reason.
//!
//! The load conversion follows the Pico SDK and NuttX: RP2040-E1 makes the
//! counter decrement twice per watchdog tick, so one millisecond is 2,000
//! load counts when the tick generator runs at 1 MHz.

const WATCHDOG_BASE: usize = 0x4005_8000;
const CTRL: *mut u32 = WATCHDOG_BASE as *mut u32;
const LOAD: *mut u32 = (WATCHDOG_BASE + 0x04) as *mut u32;
const REASON: *const u32 = (WATCHDOG_BASE + 0x08) as *const u32;
const SCRATCH4: *mut u32 = (WATCHDOG_BASE + 0x1c) as *mut u32;
const TICK: *mut u32 = (WATCHDOG_BASE + 0x2c) as *mut u32;
// RP2040 PSM layout is FRCE_ON, FRCE_OFF, WDSEL, DONE. WDSEL is therefore
// offset 0x08; offset 0x0c is the read-only DONE register.
const PSM_WDSEL_ADDR: usize = 0x4001_0008;
const PSM_WDSEL: *mut u32 = PSM_WDSEL_ADDR as *mut u32;

const CTRL_ENABLE: u32 = 1 << 30;
const CTRL_DEBUG_PAUSE: u32 = (1 << 26) | (1 << 25) | (1 << 24);
const TICK_ENABLE: u32 = 1 << 9;
const PSM_WDSEL_ALL_EXCEPT_OSCILLATORS: u32 = 0x0001_fffc;
const MAX_LOAD: u32 = 0x00ff_ffff;

/// Pico SDK's non-reboot marker. Its intentional reboot path clears this,
/// which distinguishes a timeout from UF2 flashing and ROM USB reboot.
pub const FLINT_WATCHDOG_MARKER: u32 = 0x6ab7_3121;

/// Convert milliseconds to the RP2040-E1-adjusted counter load.
pub const fn load_for_ms(timeout_ms: u32) -> u32 {
    let load = timeout_ms.saturating_mul(2_000);
    if load > MAX_LOAD {
        MAX_LOAD
    } else {
        load
    }
}

/// Start the watchdog. The debugger may optionally pause its counter.
///
/// # Safety
/// Changes reset routing and commits the chip to resetting at the timeout.
pub unsafe fn arm(timeout_ms: u32, pause_on_debug: bool) {
    unsafe {
        let mut ctrl = CTRL.read_volatile() & !CTRL_ENABLE;
        if pause_on_debug {
            ctrl |= CTRL_DEBUG_PAUSE;
        } else {
            ctrl &= !CTRL_DEBUG_PAUSE;
        }
        CTRL.write_volatile(ctrl);
        PSM_WDSEL.write_volatile(PSM_WDSEL_ALL_EXCEPT_OSCILLATORS);
        // clk_tick is clk_ref / cycles. The kernel establishes 12 MHz XOSC.
        TICK.write_volatile(TICK_ENABLE | 12);
        SCRATCH4.write_volatile(FLINT_WATCHDOG_MARKER);
        LOAD.write_volatile(load_for_ms(timeout_ms));
        CTRL.write_volatile(ctrl | CTRL_ENABLE);
    }
}

/// Reload the armed watchdog.
///
/// # Safety
/// Writes the live watchdog counter and must use the timeout policy of its owner.
#[inline]
pub unsafe fn feed(timeout_ms: u32) {
    unsafe { LOAD.write_volatile(load_for_ms(timeout_ms)) }
}

/// Stop the watchdog.
///
/// # Safety
/// Disables automatic recovery from a stopped kernel.
pub unsafe fn disarm() {
    unsafe { CTRL.write_volatile(CTRL.read_volatile() & !CTRL_ENABLE) }
}

/// Raw retained reason bits: timer is bit 0 and forced reset is bit 1.
///
/// # Safety
/// Reads a memory-mapped hardware register.
pub unsafe fn reset_reason() -> u32 {
    unsafe { REASON.read_volatile() & 0x3 }
}

pub const fn reset_reason_name(reason: u32) -> &'static str {
    match reason & 0x3 {
        0 => "hardware reset",
        1 => "watchdog timer",
        2 => "watchdog force",
        _ => "watchdog timer and force",
    }
}

/// Whether FlintOS armed the watchdog before the retained reset.
///
/// # Safety
/// Reads memory-mapped reason and scratch registers.
pub unsafe fn flint_watchdog_caused_reset() -> bool {
    unsafe { reset_reason() != 0 && SCRATCH4.read_volatile() == FLINT_WATCHDOG_MARKER }
}

/// Clear FlintOS's retained marker after consuming the reset report.
///
/// # Safety
/// Writes a retained scratch register used by reset handling.
pub unsafe fn clear_flint_watchdog_marker() {
    unsafe { SCRATCH4.write_volatile(0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e1_erratum_doubles_the_millisecond_load() {
        assert_eq!(load_for_ms(1), 2_000);
        assert_eq!(load_for_ms(100), 200_000);
        assert_eq!(load_for_ms(u32::MAX), MAX_LOAD);
    }

    #[test]
    fn each_hardware_reason_is_named() {
        assert_eq!(reset_reason_name(0), "hardware reset");
        assert_eq!(reset_reason_name(1), "watchdog timer");
        assert_eq!(reset_reason_name(2), "watchdog force");
        assert_eq!(reset_reason_name(3), "watchdog timer and force");
    }

    #[test]
    fn watchdog_reset_selection_uses_psm_wdsel_not_done() {
        assert_eq!(PSM_WDSEL_ADDR, 0x4001_0008);
    }

    #[test]
    fn timeout_marker_matches_pico_sdk_and_survives_only_timeout_reboots() {
        assert_eq!(FLINT_WATCHDOG_MARKER, 0x6ab7_3121);
    }
}
