// SPDX-License-Identifier: Apache-2.0

//! SPI bus abstraction.
//!
//! Wraps a [`PhysicalBus`] impl and exposes the [`Bus`] trait.
//! All transfers are capped at 64 bytes (Phase 1 limitation).

#![no_std]

use flint_api::bus::{Bus, BusConfig, BusError, BusResult, BusSpeed, PhysicalBus};

/// SPI bus abstraction.
pub struct SpiBus {
    phys: &'static dyn PhysicalBus,
    #[allow(dead_code)]
    config: BusConfig,
}

impl SpiBus {
    /// Create a new SPI bus wrapping a physical driver.
    pub fn new(phys: &'static dyn PhysicalBus, config: BusConfig) -> Self {
        Self { phys, config }
    }
}

impl Bus for SpiBus {
    fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        self.phys.raw_transfer(tx, rx)
    }

    fn write(&self, data: &[u8]) -> BusResult<()> {
        let mut rx = [0u8; 64];
        let len = data.len().min(64);
        self.phys.raw_transfer(&data[..len], &mut rx[..len])
    }

    fn read(&self, buf: &mut [u8]) -> BusResult<()> {
        let tx = [0u8; 64];
        let len = buf.len().min(64);
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

    struct MockSpi;

    impl PhysicalBus for MockSpi {
        fn init(&mut self, _: &BusConfig) -> BusResult<()> { Ok(()) }
        fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            // Echo: copy tx to rx
            let len = tx.len().min(rx.len());
            rx[..len].copy_from_slice(&tx[..len]);
            Ok(())
        }
        fn set_enabled(&mut self, _: bool) {}
    }

    #[test]
    fn spi_transfer_echo() {
        let phys: &'static dyn PhysicalBus = &MockSpi;
        let bus = SpiBus::new(phys, BusConfig::Spi {
            mosi: 23, miso: 19, sck: 18, max_speed: BusSpeed::MHz(1), mode: flint_api::bus::SpiMode::Mode0,
        });
        let mut rx = [0u8; 4];
        assert!(bus.transfer(b"data", &mut rx).is_ok());
        assert_eq!(&rx[..4], b"data");
    }

    #[test]
    fn spi_write_read() {
        let phys: &'static dyn PhysicalBus = &MockSpi;
        let bus = SpiBus::new(phys, BusConfig::Spi {
            mosi: 23, miso: 19, sck: 18, max_speed: BusSpeed::MHz(1), mode: flint_api::bus::SpiMode::Mode0,
        });
        assert!(bus.write(b"hello").is_ok());
        let mut buf = [0u8; 5];
        assert!(bus.read(&mut buf).is_ok());
        assert_eq!(&buf, &[0u8; 5]);
    }
}
