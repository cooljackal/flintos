// SPDX-License-Identifier: Apache-2.0

//! IO_MUX: per-pad configuration.
//!
//! Every ESP32 pad has one register controlling its alternate function, input
//! enable, and pull resistors. Two things about it are traps:
//!
//! 1. **The register offsets are not linear in the pin number.** GPIO0 is at
//!    0x44, GPIO1 at 0x88, GPIO3 at 0x84. The table below is the only correct
//!    way to get from a pin to its register; computing `pin * 4` produces
//!    plausible addresses that configure the wrong pad.
//! 2. **The "GPIO" function number is not uniform.** For most pads it is
//!    function 2, for a few it is 0. A driver that hardcodes one value silently
//!    puts some pads into a completely different peripheral's function.
//!
//! Both used to be duplicated, separately, inside `esp32_uart` and `esp32_spi`.
//!
//! Offsets confirmed against esp-idf `soc/io_mux_reg.h` (`GPIO_PIN_MUX_REG`).

use flint_hal::bus::{BusError, BusResult};
use flint_hal::pinmux::PinPull;

use crate::addr::IO_MUX_BASE;

/// Alternate-function select, `MCU_SEL` [14:12].
const MCU_SEL_SHIFT: u32 = 12;
const MCU_SEL_MASK: u32 = 0x7 << MCU_SEL_SHIFT;
/// `FUN_IE` — input enable. Without this the peripheral reads a dead pad.
const FUN_IE: u32 = 1 << 9;
/// `FUN_WPU` — internal pull-up.
const FUN_WPU: u32 = 1 << 8;
/// `FUN_WPD` — internal pull-down.
const FUN_WPD: u32 = 1 << 7;

/// IO_MUX register offset for a GPIO number.
///
/// `None` for GPIO 28-31, which have no IO_MUX register on this chip at all.
///
/// GPIO20 and GPIO24 *do* have registers and are listed here, even though the
/// common ESP32 packages (WROOM, WROVER, PICO-D4) do not bond them to a pin.
/// That is a package property, not a chip property — ESP32-PICO-V3 exposes
/// GPIO20 — and this crate describes the chip. A board that names a pin its
/// package does not carry is the board manifest's error to make.
pub fn offset(pin: u8) -> Option<u32> {
    Some(match pin {
        0 => 0x44,
        1 => 0x88,
        2 => 0x40,
        3 => 0x84,
        4 => 0x48,
        5 => 0x6C,
        6 => 0x60,
        7 => 0x64,
        8 => 0x68,
        9 => 0x54,
        10 => 0x58,
        11 => 0x5C,
        12 => 0x34,
        13 => 0x38,
        14 => 0x30,
        15 => 0x3C,
        16 => 0x4C,
        17 => 0x50,
        18 => 0x70,
        19 => 0x74,
        20 => 0x78,
        21 => 0x7C,
        22 => 0x80,
        23 => 0x8C,
        24 => 0x90,
        25 => 0x24,
        26 => 0x28,
        27 => 0x2C,
        32 => 0x1C,
        33 => 0x20,
        34 => 0x14,
        35 => 0x18,
        36 => 0x04,
        37 => 0x08,
        38 => 0x0C,
        39 => 0x10,
        _ => return None,
    })
}

/// The `MCU_SEL` value that puts a pad under GPIO-matrix control.
///
/// Function 2 for most pads. The exceptions are the six pads whose IO_MUX
/// function list starts with GPIO rather than a dedicated peripheral: GPIO0 and
/// GPIO2 (strapping pins), and the input-only pads 34-39, which have no
/// alternate functions at all. Confirmed against esp-idf `soc/io_mux_reg.h`
/// (`FUNC_GPIOn_GPIOn` / `PIN_FUNC_GPIO`).
pub fn gpio_function(pin: u8) -> u32 {
    match pin {
        34..=39 => 0,
        _ => 2,
    }
}

/// Whether a pad can drive an output at all.
///
/// GPIO34-39 are input-only on the classic ESP32: they have no output driver,
/// so routing an output signal there produces a pin that reads as whatever the
/// board pulls it to, with no error from the hardware.
pub fn is_input_only(pin: u8) -> bool {
    matches!(pin, 34..=39)
}

/// Configure a pad: alternate function, input enable, pull resistors.
///
/// Read-modify-write, so it does not disturb drive strength or the settings of
/// the other fields.
///
/// # Safety
/// Writes the IO_MUX register for `pin`. The caller must own that pad — two
/// drivers configuring the same pad race, and the loser gets a peripheral
/// wired somewhere it does not expect.
pub unsafe fn configure(pin: u8, func: u32, input_enable: bool, pull: PinPull) -> BusResult<()> {
    let off = offset(pin).ok_or(BusError::InvalidConfig)?;
    let reg = (IO_MUX_BASE + off) as *mut u32;
    let mut val = reg.read_volatile();

    val = (val & !MCU_SEL_MASK) | ((func << MCU_SEL_SHIFT) & MCU_SEL_MASK);

    if input_enable {
        val |= FUN_IE;
    } else {
        val &= !FUN_IE;
    }

    // Always clear both before setting one: leaving a stale pull-down under a
    // requested pull-up fights the pad instead of replacing the old setting.
    val &= !(FUN_WPU | FUN_WPD);
    match pull {
        PinPull::Up => val |= FUN_WPU,
        PinPull::Down => val |= FUN_WPD,
        PinPull::None => {}
    }

    reg.write_volatile(val);
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_not_linear_in_pin_number() {
        // The trap this table exists to avoid: pin * 4 is wrong for almost
        // every pad, and wrong in a way that lands on a real neighbouring
        // register rather than faulting.
        assert_eq!(offset(0), Some(0x44));
        assert_eq!(offset(1), Some(0x88));
        assert_eq!(offset(3), Some(0x84));
        assert_ne!(offset(1), Some(4));
        assert_ne!(offset(3), Some(12));
    }

    #[test]
    fn pins_without_a_register_are_rejected() {
        // 28-31 have no IO_MUX register on this chip.
        for pin in [28u8, 29, 30, 31] {
            assert_eq!(offset(pin), None, "GPIO{pin} has no IO_MUX register");
        }
        assert_eq!(offset(40), None);
    }

    #[test]
    fn package_absent_pins_still_have_chip_registers() {
        // GPIO20 and GPIO24 are not bonded out on WROOM/WROVER/PICO-D4, but
        // the registers exist and other packages expose them. Which pins a
        // board actually carries is the board manifest's problem, not this
        // crate's.
        assert_eq!(offset(20), Some(0x78));
        assert_eq!(offset(24), Some(0x90));
    }

    #[test]
    fn every_bonded_pin_has_a_distinct_offset() {
        // A duplicated offset would silently make two pins the same pad.
        let mut seen = [false; 0x100];
        for pin in 0..=39u8 {
            if let Some(off) = offset(pin) {
                let idx = off as usize;
                assert!(!seen[idx], "GPIO{pin} reuses offset {off:#x}");
                seen[idx] = true;
            }
        }
    }

    #[test]
    fn gpio_function_is_not_uniform() {
        // Function 2 for ordinary pads, 0 for the input-only ones. Hardcoding
        // either value alone misconfigures the other group.
        assert_eq!(gpio_function(21), 2);
        assert_eq!(gpio_function(22), 2);
        assert_eq!(gpio_function(36), 0);
        assert_eq!(gpio_function(39), 0);
    }

    #[test]
    fn input_only_pins_are_flagged() {
        assert!(is_input_only(34));
        assert!(is_input_only(39));
        assert!(!is_input_only(33));
        assert!(!is_input_only(21));
    }
}
