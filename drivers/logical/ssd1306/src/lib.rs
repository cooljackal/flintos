//! SSD1306 OLED display driver (128x64, I2C or SPI).
//!
//! Layer 3 logical driver — knows nothing about the bus or MCU.
//! Provides initialisation, clear, and temperature display.

#![no_std]

use flint_api::bus::{BusHandle, BusResult};

/// SSD1306 OLED display (128x64, I2C or SPI).
pub struct Ssd1306 {
    bus: BusHandle,
    width: u8,
    height: u8,
    pages: u8,
}

const CMD_CHARGE_PUMP: u8 = 0x8D;
const CMD_COM_SCAN_DEC: u8 = 0xC8;
const CMD_DISPLAY_OFF: u8 = 0xAE;
const CMD_DISPLAY_ON: u8 = 0xAF;
const CMD_DISPLAY_RAM: u8 = 0xA4;
const CMD_MEMORY_MODE: u8 = 0x20;
const CMD_NORMAL_DISPLAY: u8 = 0xA6;
const CMD_SEG_REMAP: u8 = 0xA1;
const CMD_SET_COL_ADDR: u8 = 0x21;
const CMD_SET_COM_PINS: u8 = 0xDA;
const CMD_SET_CONTRAST: u8 = 0x81;
const CMD_SET_DISPLAY_CLOCK: u8 = 0xD5;
const CMD_SET_DISPLAY_OFFSET: u8 = 0xD3;
const CMD_SET_MUX: u8 = 0xA8;
const CMD_SET_PAGE_ADDR: u8 = 0x22;
const CMD_SET_PRECHARGE: u8 = 0xD9;
const CMD_SET_VCOM_DETECT: u8 = 0xDB;

impl Ssd1306 {
    /// Create a new SSD1306 driver.
    pub fn new(bus: BusHandle) -> Self {
        Self { bus, width: 128, height: 64, pages: 8 }
    }

    /// Initialise the display.
    pub fn init(&self) -> BusResult<()> {
        self.cmd(CMD_DISPLAY_OFF)?;
        self.cmd(CMD_SET_DISPLAY_CLOCK)?;
        self.cmd(0x80)?;
        self.cmd(CMD_SET_MUX)?;
        self.cmd(self.height - 1)?;
        self.cmd(CMD_SET_DISPLAY_OFFSET)?;
        self.cmd(0x00)?;
        self.cmd(0x40)?;
        self.cmd(CMD_CHARGE_PUMP)?;
        self.cmd(0x14)?;
        self.cmd(CMD_MEMORY_MODE)?;
        self.cmd(0x00)?;
        self.cmd(CMD_SEG_REMAP)?;
        self.cmd(CMD_COM_SCAN_DEC)?;
        self.cmd(CMD_SET_COM_PINS)?;
        self.cmd(if self.height == 64 { 0x12 } else { 0x02 })?;
        self.cmd(CMD_SET_CONTRAST)?;
        self.cmd(0xCF)?;
        self.cmd(CMD_SET_PRECHARGE)?;
        self.cmd(0xF1)?;
        self.cmd(CMD_SET_VCOM_DETECT)?;
        self.cmd(0x40)?;
        self.cmd(CMD_DISPLAY_RAM)?;
        self.cmd(CMD_NORMAL_DISPLAY)?;
        self.clear()?;
        self.cmd(CMD_DISPLAY_ON)?;
        Ok(())
    }

    /// Clear the display (fill with zeros).
    pub fn clear(&self) -> BusResult<()> {
        let total = self.width as u16 * self.pages as u16;
        self.cmd(CMD_SET_COL_ADDR)?;
        self.cmd(0)?;
        self.cmd(self.width - 1)?;
        self.cmd(CMD_SET_PAGE_ADDR)?;
        self.cmd(0)?;
        self.cmd(self.pages - 1)?;

        for _ in 0..total {
            self.data(0x00)?;
        }
        Ok(())
    }

    fn cmd(&self, byte: u8) -> BusResult<()> {
        self.bus.select()?;
        self.bus.write(&[0x00, byte])?;
        self.bus.deselect()?;
        Ok(())
    }

    fn data(&self, byte: u8) -> BusResult<()> {
        self.bus.select()?;
        self.bus.write(&[0x40, byte])?;
        self.bus.deselect()?;
        Ok(())
    }

    /// Display a temperature value.
    pub fn print_temp(&self, temp_c: f32) -> BusResult<()> {
        self.clear()?;

        let int_part = temp_c as u8;
        for page in 0..3 {
            self.cmd(CMD_SET_PAGE_ADDR)?;
            self.cmd(page)?;
            self.cmd(page)?;
            self.cmd(CMD_SET_COL_ADDR)?;
            self.cmd(0)?;
            self.cmd(127)?;

            for col in 0..128u16 {
                let byte = if int_part > 0 && col < int_part as u16 {
                    0xFF
                } else {
                    0x00
                };
                if col < 20 && page == 0 {
                    self.data(0xFF)?;
                } else {
                    self.data(byte)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use flint_api::bus::{Bus, BusSpeed, BusResult};

    struct MockDisplayBus;

    impl Bus for MockDisplayBus {
        fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            let _ = (tx, rx);
            Ok(())
        }
        fn write(&self, _data: &[u8]) -> BusResult<()> { Ok(()) }
        fn read(&self, _buf: &mut [u8]) -> BusResult<()> { Ok(()) }
        fn set_speed(&self, _speed: BusSpeed) -> BusResult<()> { Err(flint_api::bus::BusError::InvalidConfig) }
        fn select(&self) -> BusResult<()> { Ok(()) }
        fn deselect(&self) -> BusResult<()> { Ok(()) }
    }

    static MOCK_BUS: MockDisplayBus = MockDisplayBus;

    #[test]
    fn ssd1306_init_ok() {
        let handle = BusHandle::new(&MOCK_BUS);
        let display = Ssd1306::new(handle);
        assert!(display.init().is_ok());
    }

    #[test]
    fn ssd1306_clear() {
        let handle = BusHandle::new(&MOCK_BUS);
        let display = Ssd1306::new(handle);
        assert!(display.clear().is_ok());
    }

    #[test]
    fn ssd1306_print_temp() {
        let handle = BusHandle::new(&MOCK_BUS);
        let display = Ssd1306::new(handle);
        assert!(display.print_temp(25.5).is_ok());
    }
}
