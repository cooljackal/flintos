// SPDX-License-Identifier: Apache-2.0

//! UART bus abstraction.
//!
//! Wraps a [`PhysicalBus`] impl and exposes the [`Bus`] trait.
//! All transfers are capped at 256 bytes (Phase 1 limitation).

#![no_std]

use api::bus::{Bus, BusError, BusResult, BusSpeed};
use api::PhysicalBus;

/// UART bus abstraction.
pub struct UartBus {
    phys: &'static dyn PhysicalBus,
}

impl UartBus {
    /// Create a new UART bus wrapping a physical driver.
    pub fn new(phys: &'static dyn PhysicalBus) -> Self {
        Self { phys }
    }
}

impl Bus for UartBus {
    fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        self.phys.raw_transfer(tx, rx)
    }

    fn write(&self, data: &[u8]) -> BusResult<()> {
        let mut rx = [0u8; 256];
        let len = data.len().min(256);
        self.phys.raw_transfer(&data[..len], &mut rx[..len])
    }

    fn read(&self, buf: &mut [u8]) -> BusResult<()> {
        let tx = [0u8; 256];
        let len = buf.len().min(256);
        self.phys.raw_transfer(&tx[..len], &mut buf[..len])
    }

    fn set_speed(&self, _speed: BusSpeed) -> BusResult<()> {
        Err(BusError::InvalidConfig)
    }

    fn select(&self) -> BusResult<()> {
        Ok(())
    }

    fn deselect(&self) -> BusResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use api::bus::{BusConfig, BusSpeed};

    struct MockUart;

    impl PhysicalBus for MockUart {
        fn init(&mut self, _: &BusConfig) -> BusResult<()> { Ok(()) }
        fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            let len = tx.len().min(rx.len());
            rx[..len].copy_from_slice(&tx[..len]);
            Ok(())
        }
        fn set_enabled(&mut self, _: bool) {}
    }

    #[test]
    fn uart_write_echo() {
        let phys: &'static dyn PhysicalBus = &MockUart;
        let bus = UartBus::new(phys);
        assert!(bus.write(b"hello").is_ok());
    }

    #[test]
    fn uart_read_zeros() {
        let phys: &'static dyn PhysicalBus = &MockUart;
        let bus = UartBus::new(phys);
        let mut buf = [0u8; 4];
        assert!(bus.read(&mut buf).is_ok());
        assert_eq!(&buf, &[0u8; 4]);
    }

    #[test]
    fn uart_set_speed_not_supported() {
        let phys: &'static dyn PhysicalBus = &MockUart;
        let bus = UartBus::new(phys);
        assert_eq!(bus.set_speed(BusSpeed::MHz(1)), Err(BusError::InvalidConfig));
    }
}
