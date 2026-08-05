#![no_std]

use flint_hal::bus::{BusConfig, BusError, BusResult, PhysicalBus, SpiMode};

/// ESP32 SPI2/SPI3 physical driver (polled mode).
pub struct Esp32Spi {
    base: u32,
}

const SPI_CMD: u32 = 0x00;
const SPI_ADDR: u32 = 0x04;
const SPI_CTRL: u32 = 0x08;
const SPI_CLOCK: u32 = 0x0C;
const SPI_USER: u32 = 0x10;
const SPI_USER1: u32 = 0x14;
const SPI_PIN: u32 = 0x18;
const SPI_SLAVE: u32 = 0x1C;
const SPI_DATA_BUF: u32 = 0x80; // 16 words (64 bytes)

const SPI_USR_COMMAND: u32 = 1 << 31;
const SPI_USR_ADDR: u32 = 1 << 30;
const SPI_USR_DUMMY: u32 = 1 << 29;
const SPI_USR_MISO: u32 = 1 << 28;
const SPI_USR_MOSI: u32 = 1 << 27;

impl Esp32Spi {
    pub fn new(base_addr: u32) -> Self {
        Self { base: base_addr }
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    /// Perform a polled SPI transfer (up to 64 bytes).
    pub fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        let len = tx.len().min(rx.len()).min(64);
        if len == 0 {
            return Ok(());
        }

        unsafe {
            // Write TX data into the data buffer.
            for i in 0..len {
                let word_addr = (self.base + SPI_DATA_BUF + (i as u32 * 4)) as *mut u32;
                word_addr.write_volatile(tx[i] as u32);
            }

            // Configure the transfer: MOSI + MISO, N bits.
            let user1 = self.reg(SPI_USER1);
            user1.write_volatile(((len as u32) * 8 - 1) & 0x1FFF); // bits to transfer

            let user = self.reg(SPI_USER);
            user.write_volatile(SPI_USR_MOSI | SPI_USR_MISO);

            // Start the transfer.
            let cmd = self.reg(SPI_CMD);
            cmd.write_volatile(1); // SPI_USR

            // Wait for completion.
            while cmd.read_volatile() & 1 != 0 {}

            // Read RX data.
            for i in 0..len {
                let word_addr = (self.base + SPI_DATA_BUF + (i as u32 * 4)) as *mut u32;
                rx[i] = word_addr.read_volatile() as u8;
            }
        }

        Ok(())
    }
}

impl PhysicalBus for Esp32Spi {
    fn init(&mut self, config: &BusConfig) -> BusResult<()> {
        match config {
            BusConfig::Spi { max_speed, mode, .. } => {
                let apb_hz: u32 = 80_000_000;
                let speed_hz = max_speed.hz();
                let div = (apb_hz / speed_hz).max(2);

                unsafe {
                    // Clock configuration.
                    self.reg(SPI_CLOCK).write_volatile(
                        ((div / 2) << 12) |         // clkcnt_N
                        ((div / 2) << 6) |          // clkcnt_H
                        ((div - 1) & 0x3F)          // clkcnt_L
                    );

                    // SPI mode (CPOL, CPHA).
                    let (cpol, cpha) = match mode {
                        SpiMode::Mode0 => (0, 0),
                        SpiMode::Mode1 => (0, 1),
                        SpiMode::Mode2 => (1, 0),
                        SpiMode::Mode3 => (1, 1),
                    };

                    let mut pin = self.reg(SPI_PIN).read_volatile();
                    if cpol != 0 { pin |= 1 << 2; } else { pin &= !(1 << 2); }
                    if cpha != 0 { pin |= 1 << 1; } else { pin &= !(1 << 1); }
                    self.reg(SPI_PIN).write_volatile(pin);

                    // Enable master mode, disable slave.
                    let slave = self.reg(SPI_SLAVE);
                    slave.write_volatile(slave.read_volatile() & !1);
                }
                Ok(())
            }
            _ => Err(BusError::InvalidConfig),
        }
    }

    fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        self.transfer(tx, rx)
    }

    fn set_enabled(&mut self, _enabled: bool) {}
}
