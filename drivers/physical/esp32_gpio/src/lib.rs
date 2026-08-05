// SPDX-License-Identifier: Apache-2.0

#![no_std]

use flint_hal::bus::{BusConfig, BusError, BusResult, PhysicalBus};

/// ESP32 GPIO driver (pins 0-31).
/// Base address: 0x3FF44000.
pub struct Esp32Gpio {
    base: u32,
}

const GPIO_OUT: u32 = 0x04;
const GPIO_OUT_W1TS: u32 = 0x08;
const GPIO_OUT_W1TC: u32 = 0x0C;
const GPIO_ENABLE: u32 = 0x10;
const GPIO_ENABLE_W1TS: u32 = 0x14;
const GPIO_ENABLE_W1TC: u32 = 0x18;
const GPIO_IN: u32 = 0x1C;
const GPIO_STATUS: u32 = 0x24;
const GPIO_STATUS_W1TC: u32 = 0x28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinMode {
    Input,
    Output,
    InputPullUp,
    InputPullDown,
    OutputOpenDrain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinLevel {
    Low = 0,
    High = 1,
}

impl Esp32Gpio {
    pub fn new(base_addr: u32) -> Self {
        Self { base: base_addr }
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    /// Set pin direction.
    pub fn set_mode(&self, pin: u8, mode: PinMode) {
        match mode {
            PinMode::Output | PinMode::OutputOpenDrain => {
                unsafe { self.reg(GPIO_ENABLE_W1TS).write_volatile(1u32 << pin); }
            }
            PinMode::Input | PinMode::InputPullUp | PinMode::InputPullDown => {
                unsafe { self.reg(GPIO_ENABLE_W1TC).write_volatile(1u32 << pin); }
            }
        }
        // Pin pad configuration (pull-up/down, open drain) would go through
        // RTC_IO or GPIO_PIN registers in a full implementation.
        let _ = mode;
    }

    /// Set pin high or low.
    pub fn write(&self, pin: u8, level: PinLevel) {
        match level {
            PinLevel::High => unsafe { self.reg(GPIO_OUT_W1TS).write_volatile(1u32 << pin); },
            PinLevel::Low => unsafe { self.reg(GPIO_OUT_W1TC).write_volatile(1u32 << pin); },
        }
    }

    /// Read pin level.
    pub fn read(&self, pin: u8) -> PinLevel {
        let val = unsafe { self.reg(GPIO_IN).read_volatile() };
        if val & (1u32 << pin) != 0 {
            PinLevel::High
        } else {
            PinLevel::Low
        }
    }

    /// Clear interrupt status for a pin.
    pub fn clear_interrupt(&self, pin: u8) {
        unsafe { self.reg(GPIO_STATUS_W1TC).write_volatile(1u32 << pin); }
    }
}

impl PhysicalBus for Esp32Gpio {
    fn init(&mut self, _config: &BusConfig) -> BusResult<()> {
        // GPIO doesn't use a typical bus config — Phase 6 initialisation
        // is driven by the board manifest.
        Ok(())
    }

    fn raw_transfer(&self, _tx: &[u8], _rx: &mut [u8]) -> BusResult<()> {
        Err(BusError::InvalidConfig)
    }

    fn set_enabled(&mut self, _enabled: bool) {}
}
