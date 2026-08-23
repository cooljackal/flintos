// SPDX-License-Identifier: Apache-2.0

#![no_std]

use soc_rp2040::{
    IO_BANK0_BASE, PADS_BANK0_BASE, RESET_IO_BANK0, RESET_PADS_BANK0, SIO_BASE,
    ctrl::{self, GpioPort},
    unreset,
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

pub struct Rp2040Pin {
    pin: u8,
}

fn bit(pin: u8) -> hal::Result<u32> {
    if pin < 30 {
        Ok(1 << pin)
    } else {
        Err(hal::Error::Unsupported)
    }
}

impl Rp2040Pin {
    pub fn open(port: &GpioPort) -> hal::Result<Self> {
        bit(port.pin)?;
        if !ctrl::claim_gpio(port.pin) {
            return Err(hal::Error::Other("RP2040 GPIO pin already in use"));
        }
        Ok(Self { pin: port.pin })
    }

    pub fn set_mode(&self, mode: PinMode) -> hal::Result<()> {
        let pin = self.pin;
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

    pub fn write(&self, level: PinLevel) -> hal::Result<()> {
        let offset = if level == PinLevel::High { 0x14 } else { 0x18 };
        unsafe { ((SIO_BASE + offset) as *mut u32).write_volatile(bit(self.pin)?) };
        Ok(())
    }

    pub fn read(&self) -> hal::Result<PinLevel> {
        let value = unsafe { ((SIO_BASE + 0x04) as *const u32).read_volatile() };
        Ok(if value & bit(self.pin)? != 0 {
            PinLevel::High
        } else {
            PinLevel::Low
        })
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
