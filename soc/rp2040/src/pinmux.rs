// SPDX-License-Identifier: Apache-2.0

//! RP2040 fixed-function pin routing.

use hal::bus::{BusError, BusResult};
use hal::pinmux::{PinConfig, PinDrive, PinMux, PinPull, Signal};

use crate::{unreset, IO_BANK0_BASE, PADS_BANK0_BASE, RESET_IO_BANK0, RESET_PADS_BANK0};

#[derive(Debug, Clone, Copy, Default)]
pub struct Rp2040PinMux;

impl Rp2040PinMux {
    pub const fn new() -> Self {
        Self
    }
}

fn uart_signal(pin: u8) -> Option<Signal> {
    if pin >= 30 {
        return None;
    }
    let instance = (pin.wrapping_add(4) / 8) & 1;
    Some(match pin & 3 {
        0 => Signal::UartTx(instance),
        1 => Signal::UartRx(instance),
        2 => Signal::UartCts(instance),
        _ => Signal::UartRts(instance),
    })
}

fn function(signal: Signal, pin: u8) -> Option<u32> {
    (uart_signal(pin) == Some(signal)).then_some(2)
}

impl PinMux for Rp2040PinMux {
    fn can_route(&self, signal: Signal, pin: u8) -> BusResult<()> {
        function(signal, pin)
            .map(|_| ())
            .ok_or(BusError::InvalidConfig)
    }

    fn route(&self, signal: Signal, pin: u8, config: PinConfig) -> BusResult<()> {
        let func = function(signal, pin).ok_or(BusError::InvalidConfig)?;
        if config.drive == PinDrive::OpenDrain {
            return Err(BusError::InvalidConfig);
        }
        unsafe {
            unreset(RESET_IO_BANK0 | RESET_PADS_BANK0);
            let pad = (PADS_BANK0_BASE + 4 + u32::from(pin) * 4) as *mut u32;
            let mut value = pad.read_volatile() & !((1 << 7) | (1 << 3) | (1 << 2));
            value |= 1 << 6;
            value |= match config.pull {
                PinPull::None => 0,
                PinPull::Up => 1 << 3,
                PinPull::Down => 1 << 2,
            };
            pad.write_volatile(value);
            ((IO_BANK0_BASE + 4 + u32::from(pin) * 8) as *mut u32).write_volatile(func);
        }
        Ok(())
    }

    fn is_native(&self, signal: Signal, pin: u8) -> bool {
        function(signal, pin).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uart_function_table_matches_rp2040_datasheet() {
        assert_eq!(uart_signal(0), Some(Signal::UartTx(0)));
        assert_eq!(uart_signal(1), Some(Signal::UartRx(0)));
        assert_eq!(uart_signal(4), Some(Signal::UartTx(1)));
        assert_eq!(uart_signal(9), Some(Signal::UartRx(1)));
        assert_eq!(uart_signal(13), Some(Signal::UartRx(0)));
        assert_eq!(uart_signal(25), Some(Signal::UartRx(1)));
        assert_eq!(uart_signal(29), Some(Signal::UartRx(0)));
        assert_eq!(uart_signal(30), None);
    }

    #[test]
    fn rejects_signals_on_pins_without_that_fixed_function() {
        let mux = Rp2040PinMux::new();
        assert!(mux.can_route(Signal::UartTx(0), 0).is_ok());
        assert!(mux.can_route(Signal::UartTx(0), 4).is_err());
        assert!(mux.can_route(Signal::SpiMosi(0), 0).is_err());
    }
}
