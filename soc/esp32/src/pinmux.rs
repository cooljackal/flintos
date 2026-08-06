// SPDX-License-Identifier: Apache-2.0

//! [`Esp32PinMux`] — the chip's implementation of [`PinMux`].
//!
//! Drivers ask for "SDA of I2C0 on GPIO 21" and this decides how to deliver it:
//! IO_MUX direct where the pad is native to the signal, the GPIO matrix
//! otherwise, and an error where the silicon cannot do it at all.

use flint_hal::bus::{BusError, BusResult};
use flint_hal::pinmux::{PinConfig, PinMux, Signal, SignalDirection};

use crate::{gpio_matrix, io_mux};

/// ESP32 pin routing.
///
/// Zero-sized: routing touches fixed register blocks, so there is no per-
/// instance state and no reason to make callers thread one around.
#[derive(Debug, Clone, Copy, Default)]
pub struct Esp32PinMux;

impl Esp32PinMux {
    pub const fn new() -> Self {
        Self
    }
}

/// The IO_MUX-native pad and alternate-function number for a signal, if it has
/// one.
///
/// A native pad reaches the peripheral without going through the matrix, which
/// saves a couple of cycles and is required to run SPI at its highest clock.
/// Most signals have one; I2C has none at all, which is why every ESP32 I2C bus
/// goes through the matrix.
///
/// Pin numbers and function values confirmed against esp-idf
/// `soc/io_mux_reg.h`.
fn native_pad(signal: Signal) -> Option<(u8, u32)> {
    Some(match signal {
        // UART. Function 0 on every UART pad.
        Signal::UartTx(0) => (1, 0),
        Signal::UartRx(0) => (3, 0),
        Signal::UartTx(1) => (10, 0),
        Signal::UartRx(1) => (9, 0),
        Signal::UartTx(2) => (17, 0),
        Signal::UartRx(2) => (16, 0),

        // SPI2 ("HSPI"), function 1.
        Signal::SpiMosi(2) => (13, 1),
        Signal::SpiMiso(2) => (12, 1),
        Signal::SpiSck(2) => (14, 1),
        Signal::SpiCs(2) => (15, 1),

        // SPI3 ("VSPI"), function 1.
        Signal::SpiMosi(3) => (23, 1),
        Signal::SpiMiso(3) => (19, 1),
        Signal::SpiSck(3) => (18, 1),
        Signal::SpiCs(3) => (5, 1),

        // I2C has no native pads on the classic ESP32. Deliberately absent
        // rather than guessed: GPIO21/22 are the conventional choice on dev
        // boards, but they are conventional, not native, and they reach the
        // controller through the matrix like any other pad.
        _ => return None,
    })
}

/// Whether the peripheral drives the pad for this signal.
fn drives_out(signal: Signal) -> bool {
    matches!(
        signal.direction(),
        SignalDirection::Output | SignalDirection::Bidirectional
    )
}

/// Whether the peripheral reads the pad for this signal.
fn reads_in(signal: Signal) -> bool {
    matches!(
        signal.direction(),
        SignalDirection::Input | SignalDirection::Bidirectional
    )
}

impl PinMux for Esp32PinMux {
    fn can_route(&self, signal: Signal, pin: u8) -> BusResult<()> {
        if pin > crate::MAX_GPIO {
            return Err(BusError::InvalidConfig);
        }
        // A pad that does not exist has no IO_MUX register; catch it here
        // rather than letting a routing write land on a neighbour.
        io_mux::offset(pin).ok_or(BusError::InvalidConfig)?;

        if drives_out(signal) && io_mux::is_input_only(pin) {
            // GPIO34-39 have no output driver. The hardware will not complain;
            // the pin simply sits at whatever the board pulls it to.
            return Err(BusError::InvalidConfig);
        }

        // The native path needs no matrix entry; anything else does.
        if native_pad(signal).is_some_and(|(native, _)| native == pin) {
            return Ok(());
        }
        gpio_matrix::signal_index(signal).ok_or(BusError::InvalidConfig)?;
        Ok(())
    }

    fn route(&self, signal: Signal, pin: u8, config: PinConfig) -> BusResult<()> {
        self.can_route(signal, pin)?;

        let reads_in = reads_in(signal);
        let drives_out = drives_out(signal);

        // Fast path: the pad is native to this signal, so IO_MUX connects it
        // directly and the matrix is not involved.
        if let Some((native_pin, func)) = native_pad(signal) {
            if native_pin == pin {
                unsafe {
                    gpio_matrix::set_drive(pin, config.drive)?;
                    io_mux::configure(pin, func, reads_in, config.pull)?;
                }
                return Ok(());
            }
        }

        // Matrix path. `can_route` already established the index exists.
        let idx = gpio_matrix::signal_index(signal).ok_or(BusError::InvalidConfig)?;

        unsafe {
            // Order matters. Set the pad's drive mode before connecting the
            // peripheral to it: an I2C controller whose output lands on a
            // still-push-pull pad drives the bus high, and if another device
            // is holding it low that is a short across the two drivers for as
            // long as the window lasts.
            gpio_matrix::set_drive(pin, config.drive)?;

            // The pad has to be in GPIO function for the matrix to own it, and
            // the function number for that is not the same on every pad.
            io_mux::configure(pin, io_mux::gpio_function(pin), reads_in, config.pull)?;

            if reads_in {
                gpio_matrix::connect_input(idx, pin, false)?;
            }
            if drives_out {
                // Output enable stays with the peripheral. For an open-drain
                // line that is the whole mechanism: the controller releases
                // the line by de-asserting its own OE, which is how it emits a
                // logical 1 and how a slave gets to stretch the clock.
                gpio_matrix::connect_output(pin, idx, true, false)?;
            }
        }

        Ok(())
    }

    fn is_native(&self, signal: Signal, pin: u8) -> bool {
        native_pad(signal).is_some_and(|(native, _)| native == pin)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i2c_has_no_native_pads() {
        // The fact that motivates the whole matrix path. GPIO21/22 are
        // conventional on dev boards, not native.
        assert_eq!(native_pad(Signal::I2cSda(0)), None);
        assert_eq!(native_pad(Signal::I2cScl(0)), None);
        let mux = Esp32PinMux::new();
        assert!(!mux.is_native(Signal::I2cSda(0), 21));
        assert!(!mux.is_native(Signal::I2cScl(0), 22));
    }

    #[test]
    fn uart0_native_pads_are_the_console_defaults() {
        assert_eq!(native_pad(Signal::UartTx(0)), Some((1, 0)));
        assert_eq!(native_pad(Signal::UartRx(0)), Some((3, 0)));
        let mux = Esp32PinMux::new();
        assert!(mux.is_native(Signal::UartTx(0), 1));
        assert!(!mux.is_native(Signal::UartTx(0), 2));
    }

    #[test]
    fn vspi_native_pads_match_the_wrover_manifest() {
        assert_eq!(native_pad(Signal::SpiMosi(3)), Some((23, 1)));
        assert_eq!(native_pad(Signal::SpiMiso(3)), Some((19, 1)));
        assert_eq!(native_pad(Signal::SpiSck(3)), Some((18, 1)));
    }

    #[test]
    fn hspi_and_vspi_do_not_share_pads() {
        for sig in [Signal::SpiMosi(2), Signal::SpiMiso(2), Signal::SpiSck(2)] {
            let (hspi_pin, _) = native_pad(sig).unwrap();
            for other in [Signal::SpiMosi(3), Signal::SpiMiso(3), Signal::SpiSck(3)] {
                let (vspi_pin, _) = native_pad(other).unwrap();
                assert_ne!(hspi_pin, vspi_pin);
            }
        }
    }

    #[test]
    fn can_route_rejects_unbonded_and_input_only_pads() {
        let mux = Esp32PinMux::new();
        // GPIO28-31 have no IO_MUX register at all.
        assert!(mux.can_route(Signal::I2cSda(0), 29).is_err());
        // Past the end of the GPIO range entirely.
        assert!(mux.can_route(Signal::I2cSda(0), 40).is_err());
        // GPIO34-39 are input-only; an I2C line must be able to drive low.
        assert!(mux.can_route(Signal::I2cSda(0), 34).is_err());
        // ...but an input-only signal is fine there.
        assert!(mux.can_route(Signal::SpiMiso(3), 34).is_ok());
    }

    #[test]
    fn can_route_accepts_i2c_on_the_conventional_dev_board_pins() {
        // Not native, but reachable through the matrix -- which is the whole
        // point of this layer. These used to be rejected outright.
        let mux = Esp32PinMux::new();
        assert!(mux.can_route(Signal::I2cSda(0), 21).is_ok());
        assert!(mux.can_route(Signal::I2cScl(0), 22).is_ok());
        // And the M5Stack Atom's Grove port, which uses entirely different pins.
        assert!(mux.can_route(Signal::I2cSda(0), 26).is_ok());
        assert!(mux.can_route(Signal::I2cScl(0), 32).is_ok());
    }

    #[test]
    fn can_route_rejects_absent_controller_instances() {
        let mux = Esp32PinMux::new();
        assert!(mux.can_route(Signal::I2cSda(2), 21).is_err());
        assert!(mux.can_route(Signal::UartTx(3), 21).is_err());
    }

    #[test]
    fn native_pads_are_all_bonded_out() {
        for sig in [
            Signal::UartTx(0),
            Signal::UartRx(2),
            Signal::SpiMosi(2),
            Signal::SpiCs(3),
        ] {
            let (pin, _) = native_pad(sig).unwrap();
            assert!(
                io_mux::offset(pin).is_some(),
                "{sig:?} claims a native pad that has no IO_MUX register"
            );
        }
    }
}
