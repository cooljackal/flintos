// SPDX-License-Identifier: Apache-2.0

//! The GPIO matrix: routing peripheral signals to arbitrary pads.
//!
//! This is what makes the ESP32 unusual. Rather than each pad having a short
//! fixed list of functions, a 256×40 crossbar connects almost any peripheral
//! signal to almost any pad, in either direction. It costs a couple of clock
//! cycles of latency compared to the IO_MUX direct path, which is why the
//! high-speed peripherals also have "native" pads that bypass it.
//!
//! Two register files, indexed differently, which is the easiest thing here to
//! get wrong:
//!
//! - `GPIO_FUNCn_IN_SEL_CFG_REG` at `0x130 + 4n`, where **n is the signal
//!   index**. Says which pad a peripheral input reads from.
//! - `GPIO_FUNCn_OUT_SEL_CFG_REG` at `0x530 + 4n`, where **n is the GPIO
//!   number**. Says which peripheral signal a pad drives.
//!
//! The 256 input registers occupy `0x130..0x530`, which is exactly where the
//! output registers begin — a useful check that both constants are right.
//!
//! Confirmed against esp-idf `soc/gpio_reg.h` and `soc/gpio_sig_map.h`.

use flint_hal::bus::{BusError, BusResult};
use flint_hal::pinmux::{PinDrive, Signal};

use crate::addr::GPIO_BASE;

// ── Register offsets ────────────────────────────────────────────────────────

/// `GPIO_FUNC0_IN_SEL_CFG_REG`, indexed by signal.
const FUNC_IN_SEL_CFG: u32 = 0x130;
/// `GPIO_FUNC0_OUT_SEL_CFG_REG`, indexed by GPIO number.
const FUNC_OUT_SEL_CFG: u32 = 0x530;
/// `GPIO_PIN0_REG`, indexed by GPIO number. Holds `PAD_DRIVER`.
const PIN_REG: u32 = 0x88;

/// Number of routable input signals.
const NUM_SIGNALS: u32 = 256;

// IN_SEL_CFG fields.
const IN_SEL_MASK: u32 = 0x3F; // [5:0] — source GPIO
const IN_INV_SEL: u32 = 1 << 6;
/// `SIG_IN_SEL`: 1 = take the signal from the matrix, 0 = from IO_MUX direct.
const SIG_IN_SEL: u32 = 1 << 7;

// OUT_SEL_CFG fields.
const OUT_SEL_MASK: u32 = 0x1FF; // [8:0] — peripheral output signal index
const OUT_INV_SEL: u32 = 1 << 9;
/// `OEN_SEL`: 1 = output enable comes from `GPIO_ENABLE`, 0 = from the
/// peripheral itself.
const OEN_SEL: u32 = 1 << 10;
const OEN_INV_SEL: u32 = 1 << 11;

// PIN_REG fields.
/// `PAD_DRIVER`: 1 = open drain, 0 = push-pull.
const PAD_DRIVER: u32 = 1 << 2;

/// The `OUT_SEL` value meaning "this pad is driven by the `GPIO_OUT` register",
/// i.e. it is a plain software-controlled GPIO rather than a peripheral pin.
pub const SIG_GPIO_OUT: u32 = 256;

/// `IN_SEL` value that feeds a peripheral input a constant 0.
pub const IN_CONST_ZERO: u32 = 0x30;
/// `IN_SEL` value that feeds a peripheral input a constant 1.
pub const IN_CONST_ONE: u32 = 0x38;

// ── Signal index map ────────────────────────────────────────────────────────

/// Peripheral signal index for a [`Signal`], as used by both register files.
///
/// `None` if this chip has no such controller. Values from esp-idf
/// `soc/gpio_sig_map.h`; input and output indices coincide for every signal
/// below, which is true on the classic ESP32 but is not a general rule.
pub fn signal_index(signal: Signal) -> Option<u32> {
    Some(match signal {
        // UART. U2 is far away in the map, which is not a typo.
        Signal::UartRx(0) | Signal::UartTx(0) => 14,
        Signal::UartCts(0) | Signal::UartRts(0) => 15,
        Signal::UartRx(1) | Signal::UartTx(1) => 17,
        Signal::UartCts(1) | Signal::UartRts(1) => 18,
        Signal::UartRx(2) | Signal::UartTx(2) => 198,
        Signal::UartCts(2) | Signal::UartRts(2) => 199,

        // I2C. These have no IO_MUX-native pads at all, so every ESP32 I2C
        // bus that has ever worked went through this table.
        Signal::I2cScl(0) => 29,
        Signal::I2cSda(0) => 30,
        Signal::I2cScl(1) => 95,
        Signal::I2cSda(1) => 96,

        // SPI2 ("HSPI").
        Signal::SpiSck(2) => 8,
        Signal::SpiMiso(2) => 9,
        Signal::SpiMosi(2) => 10,
        Signal::SpiCs(2) => 11,

        // SPI3 ("VSPI"). Note CS is 68, not 66 or 67 -- those are HD and WP.
        Signal::SpiSck(3) => 63,
        Signal::SpiMiso(3) => 64,
        Signal::SpiMosi(3) => 65,
        Signal::SpiCs(3) => 68,

        _ => return None,
    })
}

// ── Routing ─────────────────────────────────────────────────────────────────

fn reg(offset: u32) -> *mut u32 {
    (GPIO_BASE + offset) as *mut u32
}

/// Route a peripheral *input* so it reads from `pin`.
///
/// # Safety
/// Writes the matrix register for `signal_idx`. Two peripherals cannot share
/// an input signal index, so the caller must own the controller.
pub unsafe fn connect_input(signal_idx: u32, pin: u8, invert: bool) -> BusResult<()> {
    if signal_idx >= NUM_SIGNALS || pin as u32 > IN_SEL_MASK {
        return Err(BusError::InvalidConfig);
    }
    let mut val = (pin as u32) | SIG_IN_SEL;
    if invert {
        val |= IN_INV_SEL;
    }
    reg(FUNC_IN_SEL_CFG + signal_idx * 4).write_volatile(val);
    Ok(())
}

/// Route a peripheral *output* so it drives `pin`.
///
/// `peripheral_oe` selects who controls the pad's output enable. For a normal
/// output the peripheral does, and this is `true`. For an open-drain line
/// driven by a peripheral -- I2C -- it is also the peripheral: the controller
/// releases the line by de-asserting its own output enable, which is exactly
/// how it generates a logical 1 and how it lets a slave stretch the clock.
/// Forcing `OEN_SEL` on instead would hand output-enable to the `GPIO_ENABLE`
/// register and pin the line permanently driven or permanently released.
///
/// # Safety
/// Writes the matrix register for `pin`.
pub unsafe fn connect_output(
    pin: u8,
    signal_idx: u32,
    peripheral_oe: bool,
    invert: bool,
) -> BusResult<()> {
    if signal_idx > OUT_SEL_MASK {
        return Err(BusError::InvalidConfig);
    }
    let mut val = signal_idx & OUT_SEL_MASK;
    if invert {
        val |= OUT_INV_SEL;
    }
    if !peripheral_oe {
        val |= OEN_SEL;
    }
    let _ = OEN_INV_SEL; // documented above; no caller needs inverted OE yet
    reg(FUNC_OUT_SEL_CFG + pin as u32 * 4).write_volatile(val);
    Ok(())
}

/// Set a pad's output driver to push-pull or open-drain.
///
/// # Safety
/// Writes `GPIO_PINn_REG` for `pin`, read-modify-write so the interrupt
/// configuration in the same register survives.
pub unsafe fn set_drive(pin: u8, drive: PinDrive) -> BusResult<()> {
    if pin > crate::MAX_GPIO {
        return Err(BusError::InvalidConfig);
    }
    let r = reg(PIN_REG + pin as u32 * 4);
    let mut val = r.read_volatile();
    match drive {
        PinDrive::OpenDrain => val |= PAD_DRIVER,
        PinDrive::PushPull => val &= !PAD_DRIVER,
    }
    r.write_volatile(val);
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_register_files_abut_exactly() {
        // 256 input registers starting at 0x130 must end precisely where the
        // output registers begin. If either constant were wrong this would
        // not hold, and routing would write into the wrong file.
        assert_eq!(FUNC_IN_SEL_CFG + NUM_SIGNALS * 4, FUNC_OUT_SEL_CFG);
    }

    #[test]
    fn i2c_signal_indices_match_the_idf_map() {
        // The pair that unblocks I2C on this chip. SCL is the lower index.
        assert_eq!(signal_index(Signal::I2cScl(0)), Some(29));
        assert_eq!(signal_index(Signal::I2cSda(0)), Some(30));
        assert_eq!(signal_index(Signal::I2cScl(1)), Some(95));
        assert_eq!(signal_index(Signal::I2cSda(1)), Some(96));
    }

    #[test]
    fn uart2_is_not_adjacent_to_uart0_and_uart1() {
        assert_eq!(signal_index(Signal::UartTx(0)), Some(14));
        assert_eq!(signal_index(Signal::UartTx(1)), Some(17));
        assert_eq!(signal_index(Signal::UartTx(2)), Some(198));
    }

    #[test]
    fn vspi_cs_is_68_not_the_next_index_after_mosi() {
        // 66 and 67 are VSPIHD and VSPIWP. Assuming the four SPI signals are
        // contiguous puts CS on a quad-mode data line.
        assert_eq!(signal_index(Signal::SpiMosi(3)), Some(65));
        assert_eq!(signal_index(Signal::SpiCs(3)), Some(68));
    }

    #[test]
    fn unknown_controller_instances_are_rejected() {
        assert_eq!(signal_index(Signal::I2cSda(2)), None);
        assert_eq!(signal_index(Signal::UartTx(3)), None);
        // SPI0 and SPI1 drive the boot flash and are not routable here.
        assert_eq!(signal_index(Signal::SpiSck(1)), None);
    }

    #[test]
    fn every_signal_index_fits_its_register_field() {
        for sig in [
            Signal::UartTx(0),
            Signal::UartRx(2),
            Signal::I2cSda(1),
            Signal::SpiCs(3),
        ] {
            let idx = signal_index(sig).unwrap();
            assert!(idx < NUM_SIGNALS, "{sig:?} index {idx} exceeds the matrix");
            assert!(idx <= OUT_SEL_MASK);
        }
    }

    #[test]
    fn constant_input_selectors_are_outside_the_gpio_range() {
        // 48 and 56 are not GPIO numbers; they are the matrix's way of tying
        // an unused peripheral input low or high.
        assert!(IN_CONST_ZERO > crate::MAX_GPIO as u32);
        assert!(IN_CONST_ONE > crate::MAX_GPIO as u32);
        assert!(IN_CONST_ONE <= IN_SEL_MASK);
    }

    #[test]
    fn gpio_out_signal_is_past_the_peripheral_signals() {
        assert_eq!(SIG_GPIO_OUT, NUM_SIGNALS);
        assert!(SIG_GPIO_OUT <= OUT_SEL_MASK);
    }
}
