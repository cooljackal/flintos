// SPDX-License-Identifier: Apache-2.0

#![no_std]

use soc_rp2040::{
    ctrl::{self, GpioPort},
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Falling,
    Rising,
    Both,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EdgeEvents {
    pub falling: bool,
    pub rising: bool,
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

fn interrupt_registers(pin: u8) -> hal::Result<(*mut u32, *mut u32, u32, u32)> {
    bit(pin)?;
    let group = u32::from(pin / 8) * 4;
    let shift = u32::from(pin % 8) * 4;
    let falling = 1 << (shift + 2);
    let rising = 1 << (shift + 3);
    Ok((
        (IO_BANK0_BASE + 0x00f0 + group) as *mut u32,
        (IO_BANK0_BASE + 0x0100 + group) as *mut u32,
        falling,
        rising,
    ))
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

    pub fn enable_edge_interrupt(&self, edge: Edge) -> hal::Result<()> {
        let (raw, enable, falling, rising) = interrupt_registers(self.pin)?;
        let selected = match edge {
            Edge::Falling => falling,
            Edge::Rising => rising,
            Edge::Both => falling | rising,
        };
        unsafe {
            raw.write_volatile(falling | rising);
            enable.write_volatile((enable.read_volatile() & !(falling | rising)) | selected);
        }
        Ok(())
    }

    pub fn disable_edge_interrupt(&self) -> hal::Result<()> {
        let (raw, enable, falling, rising) = interrupt_registers(self.pin)?;
        unsafe {
            enable.write_volatile(enable.read_volatile() & !(falling | rising));
            raw.write_volatile(falling | rising);
        }
        Ok(())
    }

    pub fn take_edge_events(&self) -> hal::Result<EdgeEvents> {
        let (raw, _enable, falling, rising) = interrupt_registers(self.pin)?;
        let pending = unsafe { raw.read_volatile() } & (falling | rising);
        if pending != 0 {
            unsafe { raw.write_volatile(pending) };
        }
        Ok(EdgeEvents {
            falling: pending & falling != 0,
            rising: pending & rising != 0,
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

    #[test]
    fn edge_fields_follow_the_four_bits_per_gpio_layout() {
        let (_, _, falling0, rising0) = interrupt_registers(0).unwrap();
        let (_, _, falling7, rising7) = interrupt_registers(7).unwrap();
        let (raw8, enable8, falling8, rising8) = interrupt_registers(8).unwrap();
        assert_eq!((falling0, rising0), (1 << 2, 1 << 3));
        assert_eq!((falling7, rising7), (1 << 30, 1 << 31));
        assert_eq!((falling8, rising8), (1 << 2, 1 << 3));
        assert_eq!(raw8 as usize, IO_BANK0_BASE as usize + 0xf4);
        assert_eq!(enable8 as usize, IO_BANK0_BASE as usize + 0x104);
    }
}
