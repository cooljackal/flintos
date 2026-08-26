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
const SCRATCH5: *mut u32 = (WATCHDOG_BASE + 0x20) as *mut u32;
const TICK: *mut u32 = (WATCHDOG_BASE + 0x2c) as *mut u32;
// RP2040 PSM layout is FRCE_ON, FRCE_OFF, WDSEL, DONE. WDSEL is therefore
// offset 0x08; offset 0x0c is the read-only DONE register.
const PSM_WDSEL_ADDR: usize = 0x4001_0008;
const PSM_WDSEL: *mut u32 = PSM_WDSEL_ADDR as *mut u32;

const CTRL_ENABLE: u32 = 1 << 30;
const CTRL_DEBUG_PAUSE: u32 = (1 << 26) | (1 << 25) | (1 << 24);
const TICK_ENABLE: u32 = 1 << 9;
const PSM_WDSEL_ALL_EXCEPT_OSCILLATORS: u32 = 0x0001_fffc;
const PSM_WDSEL_BOTH_PROCESSORS: u32 = (1 << 16) | (1 << 15);
const MAX_LOAD: u32 = 0x00ff_ffff;

/// Pico SDK's non-reboot marker. Its intentional reboot path clears this,
/// which distinguishes a timeout from UF2 flashing and ROM USB reboot.
pub const FLINT_WATCHDOG_MARKER: u32 = 0x6ab7_3121;
/// Requests one direct flash reboot so a panic snapshot remains in SRAM.
pub const FLINT_PANIC_REBOOT_MARKER: u32 = 0x5041_4e32; // "PAN2"

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
unsafe fn arm_with_selection(timeout_ms: u32, pause_on_debug: bool, reset_selection: u32) {
    unsafe {
        let mut ctrl = CTRL.read_volatile() & !CTRL_ENABLE;
        if pause_on_debug {
            ctrl |= CTRL_DEBUG_PAUSE;
        } else {
            ctrl &= !CTRL_DEBUG_PAUSE;
        }
        CTRL.write_volatile(ctrl);
        PSM_WDSEL.write_volatile(reset_selection);
        // clk_tick is clk_ref / cycles. The kernel establishes 12 MHz XOSC.
        TICK.write_volatile(TICK_ENABLE | 12);
        SCRATCH4.write_volatile(FLINT_WATCHDOG_MARKER);
        LOAD.write_volatile(load_for_ms(timeout_ms));
        CTRL.write_volatile(ctrl | CTRL_ENABLE);
    }
}

/// Start the watchdog. The debugger may optionally pause its counter.
///
/// # Safety
/// Changes reset routing and commits the chip to resetting at the timeout.
pub unsafe fn arm(timeout_ms: u32, pause_on_debug: bool) {
    unsafe { arm_with_selection(timeout_ms, pause_on_debug, PSM_WDSEL_ALL_EXCEPT_OSCILLATORS) }
}

/// Start a watchdog reset that reboots the application once before BOOTSEL.
///
/// # Safety
/// Commits the chip to resetting at the timeout and changes retained scratch state.
pub unsafe fn arm_panic_recovery(timeout_ms: u32) {
    unsafe {
        SCRATCH5.write_volatile(FLINT_PANIC_REBOOT_MARKER);
        // Reset both Cortex-M0+ cores but not SRAM. The early handler consumes
        // the retained snapshot before arming the ordinary full-chip watchdog.
        arm_with_selection(timeout_ms, false, PSM_WDSEL_BOTH_PROCESSORS);
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
    unsafe {
        SCRATCH4.write_volatile(0);
        SCRATCH5.write_volatile(0);
    }
}

/// Fail-closed deadline for an XIP-off ROM call. An existing watchdog is never
/// stopped or fed: CTRL.TIME is broken on RP2040 (pico-sdk issue #1492), so its
/// remaining time cannot be saved/restored. A 1 MHz existing timer is bounded
/// by the hardware's 24-bit maximum load (~8.4 seconds); a new guard uses 3 s.
#[cfg(target_arch = "arm")]
pub struct FlashDeadline {
    ctrl: u32,
    tick: u32,
    selection: u32,
    scratch4: u32,
    scratch5: u32,
}

#[cfg(target_arch = "arm")]
impl FlashDeadline {
    /// Bound one ROM operation to 3 s, or the already-running watchdog deadline.
    ///
    /// # Safety
    /// Both cores must be excluded from watchdog access until this is dropped.
    /// XIP must already be restored when dropped. clk_ref must be 12 MHz.
    pub unsafe fn begin() -> Option<Self> {
        let ctrl = CTRL.read_volatile();
        let tick = TICK.read_volatile() & 0x3ff;
        // Cannot preserve an unknown existing time base without feeding it.
        if ctrl & CTRL_ENABLE != 0 && tick != (TICK_ENABLE | 12) {
            return None;
        }
        let saved = Self {
            ctrl,
            tick,
            selection: PSM_WDSEL.read_volatile(),
            scratch4: SCRATCH4.read_volatile(),
            scratch5: SCRATCH5.read_volatile(),
        };
        // Atomic aliases do not stop/reload an already-running counter.
        (0x4005_b000 as *mut u32).write_volatile(CTRL_DEBUG_PAUSE);
        PSM_WDSEL.write_volatile(PSM_WDSEL_ALL_EXCEPT_OSCILLATORS);
        SCRATCH4.write_volatile(FLINT_WATCHDOG_MARKER);
        // A flash failure must not reboot into the same destructive operation.
        SCRATCH5.write_volatile(0);
        if ctrl & CTRL_ENABLE == 0 {
            TICK.write_volatile(TICK_ENABLE | 12);
            LOAD.write_volatile(load_for_ms(3_000));
            (0x4005_a000 as *mut u32).write_volatile(CTRL_ENABLE);
        }
        Some(saved)
    }
}

#[cfg(target_arch = "arm")]
impl Drop for FlashDeadline {
    fn drop(&mut self) {
        unsafe {
            if self.ctrl & CTRL_ENABLE == 0 {
                (0x4005_b000 as *mut u32).write_volatile(CTRL_ENABLE);
                TICK.write_volatile(self.tick);
            }
            PSM_WDSEL.write_volatile(self.selection);
            SCRATCH4.write_volatile(self.scratch4);
            SCRATCH5.write_volatile(self.scratch5);
            (0x4005_a000 as *mut u32).write_volatile(self.ctrl & CTRL_DEBUG_PAUSE);
        }
    }
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
        assert_eq!(PSM_WDSEL_BOTH_PROCESSORS, 0x0001_8000);
        assert_eq!(PSM_WDSEL_BOTH_PROCESSORS & 0x0000_0fc0, 0);
    }

    #[test]
    fn timeout_marker_matches_pico_sdk_and_survives_only_timeout_reboots() {
        assert_eq!(FLINT_WATCHDOG_MARKER, 0x6ab7_3121);
        assert_eq!(FLINT_PANIC_REBOOT_MARKER.to_be_bytes(), *b"PAN2");
    }
}
