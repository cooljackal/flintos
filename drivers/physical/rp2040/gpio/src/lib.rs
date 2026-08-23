// SPDX-License-Identifier: Apache-2.0

#![no_std]

use hal::bus::{BusError, BusResult};
use soc_rp2040::{
    unreset, IO_BANK0_BASE, PADS_BANK0_BASE, RESET_IO_BANK0, RESET_PADS_BANK0, SIO_BASE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinMode {
    Input,
    Output,
    InputPullUp,
    InputPullDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinLevel {
    Low,
    High,
}

pub struct Rp2040Gpio;

fn bit(pin: u8) -> BusResult<u32> {
    if pin < 30 {
        Ok(1 << pin)
    } else {
        Err(BusError::InvalidConfig)
    }
}

impl Rp2040Gpio {
    pub const fn new() -> Self {
        Self
    }

    pub fn set_mode(&self, pin: u8, mode: PinMode) -> BusResult<()> {
        let mask = bit(pin)?;
        unsafe {
            unreset(RESET_IO_BANK0 | RESET_PADS_BANK0);
            let pad = (PADS_BANK0_BASE + 4 + u32::from(pin) * 4) as *mut u32;
            let mut value = pad.read_volatile() & !((1 << 7) | (1 << 3) | (1 << 2));
            value |= 1 << 6;
            value |= match mode {
                PinMode::InputPullUp => 1 << 3,
                PinMode::InputPullDown => 1 << 2,
                _ => 0,
            };
            pad.write_volatile(value);
            ((IO_BANK0_BASE + 4 + u32::from(pin) * 8) as *mut u32).write_volatile(5);
            let oe = if mode == PinMode::Output { 0x24 } else { 0x28 };
            ((SIO_BASE + oe) as *mut u32).write_volatile(mask);
        }
        Ok(())
    }

    pub fn write(&self, pin: u8, level: PinLevel) -> BusResult<()> {
        let offset = if level == PinLevel::High { 0x14 } else { 0x18 };
        unsafe { ((SIO_BASE + offset) as *mut u32).write_volatile(bit(pin)?) };
        Ok(())
    }

    pub fn read(&self, pin: u8) -> BusResult<PinLevel> {
        let value = unsafe { ((SIO_BASE + 0x04) as *const u32).read_volatile() };
        Ok(if value & bit(pin)? != 0 {
            PinLevel::High
        } else {
            PinLevel::Low
        })
    }
}

impl Default for Rp2040Gpio {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_bonded_gpio_are_accepted() {
        assert_eq!(bit(29), Ok(1 << 29));
        assert!(bit(30).is_err());
    }
}
