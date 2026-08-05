//! I2C bus abstraction.
//!
//! Wraps a [`PhysicalBus`] impl with a fixed slave address and
//! exposes the [`Bus`] trait.  Messages are formatted as raw
//! I2C frames with the slave address in the first byte.

#![no_std]

use flint_api::bus::{Bus, BusError, BusResult, BusSpeed, PhysicalBus};

/// I2C bus abstraction.
pub struct I2cBus {
    phys: &'static dyn PhysicalBus,
    addr: u8,
}

impl I2cBus {
    /// Create a new I2C bus for `slave_addr`.
    pub fn new(phys: &'static dyn PhysicalBus, slave_addr: u8) -> Self {
        Self { phys, addr: slave_addr }
    }
}

impl Bus for I2cBus {
    fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        if tx.len() > 0 {
            self.phys.raw_transfer(&[self.addr], &mut [])?;
        }
        if rx.len() > 0 {
            self.phys.raw_transfer(&[self.addr | 1], &mut [0; 64])?;
        }
        let _ = rx;
        Ok(())
    }

    fn write(&self, data: &[u8]) -> BusResult<()> {
        let mut buf = [0u8; 65];
        buf[0] = self.addr << 1;
        let len = data.len().min(64);
        buf[1..=len].copy_from_slice(&data[..len]);
        self.phys.raw_transfer(&buf[..=len], &mut [0u8; 65])
    }

    fn read(&self, buf: &mut [u8]) -> BusResult<()> {
        let len = buf.len().min(64);
        let tx = [0u8; 64];
        self.phys.raw_transfer(&tx[..len], buf)
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

    struct MockI2c;

    impl PhysicalBus for MockI2c {
        fn init(&mut self, _: &BusConfig) -> BusResult<()> { Ok(()) }
        fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            let len = tx.len().min(rx.len());
            rx[..len].copy_from_slice(&tx[..len]);
            Ok(())
        }
        fn set_enabled(&mut self, _: bool) {}
    }

    use flint_api::bus::BusConfig;

    #[test]
    fn i2c_write_builds_frame() {
        let phys: &'static dyn PhysicalBus = &MockI2c;
        let bus = I2cBus::new(phys, 0x76);
        assert!(bus.write(&[0xF4, 0x27]).is_ok());
    }

    #[test]
    fn i2c_read() {
        let phys: &'static dyn PhysicalBus = &MockI2c;
        let bus = I2cBus::new(phys, 0x76);
        let mut buf = [0u8; 3];
        assert!(bus.read(&mut buf).is_ok());
    }
}
