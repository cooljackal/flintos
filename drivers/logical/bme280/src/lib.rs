// SPDX-License-Identifier: Apache-2.0

//! BME280 temperature / humidity / pressure sensor driver.
//!
//! Layer 3 logical driver — knows nothing about the bus or MCU.
//! Communicates via a [`BusHandle`] provided at construction.

#![no_std]

use flint_api::bus::{BusHandle, BusResult};

/// BME280 temperature/humidity/pressure sensor.
pub struct Bme280 {
    bus: BusHandle,
}

// BME280 register map
const REG_ID: u8 = 0xD0;
const REG_RESET: u8 = 0xE0;
const REG_CTRL_HUM: u8 = 0xF2;
const REG_CTRL_MEAS: u8 = 0xF4;
const REG_CONFIG: u8 = 0xF5;
const REG_PRESS_MSB: u8 = 0xF7;
const CHIP_ID: u8 = 0x60;

impl Bme280 {
    /// Create a new BME280 driver on the given bus handle.
    pub fn new(bus: BusHandle) -> Self {
        Self { bus }
    }

    /// Read the chip ID to verify presence.
    pub fn chip_id(&self) -> BusResult<u8> {
        let mut buf = [0u8; 1];
        self.bus.select()?;
        self.bus.transfer(&[REG_ID], &mut buf)?;
        self.bus.deselect()?;
        Ok(buf[0])
    }

    /// Initialise the sensor.
    pub fn init(&self) -> BusResult<()> {
        let id = self.chip_id()?;
        if id != CHIP_ID {
            return Err(flint_api::bus::BusError::DeviceNotResponding);
        }

        self.bus.select()?;
        self.bus.write(&[REG_RESET, 0xB6])?;
        self.bus.deselect()?;

        self.bus.select()?;
        self.bus.write(&[REG_CTRL_HUM, 0x01])?;
        self.bus.write(&[REG_CTRL_MEAS, 0x27])?;
        self.bus.write(&[REG_CONFIG, 0xA0])?;
        self.bus.deselect()?;

        Ok(())
    }

    /// Read temperature in degrees Celsius.
    pub fn read_temperature(&self) -> BusResult<f32> {
        let mut buf = [0u8; 3];
        self.bus.select()?;
        self.bus.transfer(&[REG_PRESS_MSB], &mut buf)?;
        self.bus.deselect()?;

        let raw = ((buf[0] as u32) << 12) | ((buf[1] as u32) << 4) | ((buf[2] as u32) >> 4);
        let temp = (raw as f32) / 100.0;
        Ok(temp)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use flint_api::bus::{Bus, BusResult, BusSpeed};

    struct MockBmeBus {
        chip_id: u8,
    }

    impl Bus for MockBmeBus {
        fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            if tx.len() >= 1 && tx[0] == REG_ID && rx.len() >= 1 {
                rx[0] = self.chip_id;
            }
            Ok(())
        }
        fn write(&self, _data: &[u8]) -> BusResult<()> { Ok(()) }
        fn read(&self, _buf: &mut [u8]) -> BusResult<()> { Ok(()) }
        fn set_speed(&self, _speed: BusSpeed) -> BusResult<()> { Err(flint_api::bus::BusError::InvalidConfig) }
        fn select(&self) -> BusResult<()> { Ok(()) }
        fn deselect(&self) -> BusResult<()> { Ok(()) }
    }

    static MOCK_OK: MockBmeBus = MockBmeBus { chip_id: 0x60 };
    static MOCK_BAD: MockBmeBus = MockBmeBus { chip_id: 0xFF };

    #[test]
    fn bme280_chip_id_ok() {
        let handle = BusHandle::new(&MOCK_OK);
        let sensor = Bme280::new(handle);
        assert_eq!(sensor.chip_id(), Ok(0x60));
    }

    #[test]
    fn bme280_chip_id_wrong() {
        let handle = BusHandle::new(&MOCK_BAD);
        let sensor = Bme280::new(handle);
        assert_eq!(sensor.init(), Err(flint_api::bus::BusError::DeviceNotResponding));
    }

    #[test]
    fn bme280_read_temp() {
        let handle = BusHandle::new(&MOCK_OK);
        let sensor = Bme280::new(handle);
        let temp = sensor.read_temperature().unwrap();
        assert!(temp >= 0.0);
    }
}
