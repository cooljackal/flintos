// SPDX-License-Identifier: Apache-2.0

//! Bus abstraction traits and types.
//!
//! Three-layer bus model:
//! - **Layer 1 – PhysicalBus**: raw register-level driver (e.g. `esp32_spi`).
//! - **Layer 2 – Bus**: protocol-level abstraction (e.g. `spi_bus::SpiBus`).
//! - **Layer 3 – Logical driver**: device driver using `BusHandle` (e.g. `bme280::Bme280`).
//!
//! The `BusHandle` struct is the bridge between layers 2 and 3: it wraps
//! a `&dyn Bus` reference and provides delegation methods.

use core::fmt;

// ── Error type ─────────────────────────────────────────────────────────────-

/// Result type for bus operations.
pub type BusResult<T> = Result<T, BusError>;

/// Bus error enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusError {
    /// Operation timed out.
    Timeout,
    /// CRC or checksum mismatch.
    CrcMismatch,
    /// Device did not respond to addressing.
    DeviceNotResponding,
    /// Configuration not supported by this bus.
    InvalidConfig,
    /// DMA engine error.
    DmaError,
    /// Bus or device is busy.
    Busy,
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

// ── Speed ───────────────────────────────────────────────────────────────────

/// Bus clock / data-rate selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusSpeed {
    /// Arbitrary kHz value.
    KHz(u32),
    /// Arbitrary MHz value.
    MHz(u32),
    /// Standard I2C mode: 100 kHz.
    Standard100k,
    /// Fast I2C mode: 400 kHz.
    Fast400k,
    /// Fast Plus I2C mode: 1 MHz.
    FastPlus1M,
    /// High-speed I2C mode: 3.4 MHz.
    HighSpeed3M4,
}

impl BusSpeed {
    /// Return the speed in Hertz.
    pub fn hz(&self) -> u32 {
        match self {
            BusSpeed::KHz(v) => v * 1000,
            BusSpeed::MHz(v) => v * 1_000_000,
            BusSpeed::Standard100k => 100_000,
            BusSpeed::Fast400k => 400_000,
            BusSpeed::FastPlus1M => 1_000_000,
            BusSpeed::HighSpeed3M4 => 3_400_000,
        }
    }
}

// ─── Bus configuration ──────────────────────────────────────────────────────

/// Complete configuration for initialising a peripheral bus.
#[derive(Debug, Clone, Copy)]
pub enum BusConfig {
    /// SPI bus configuration.
    Spi {
        mosi: u8,
        miso: u8,
        sck: u8,
        max_speed: BusSpeed,
        mode: SpiMode,
    },
    /// I2C bus configuration.
    I2c {
        sda: u8,
        scl: u8,
        speed: BusSpeed,
    },
    /// UART bus configuration.
    Uart {
        tx: u8,
        rx: u8,
        baud: u32,
        data_bits: UartDataBits,
        parity: UartParity,
        stop_bits: UartStopBits,
    },
}

/// SPI clock polarity / phase modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpiMode {
    Mode0,
    Mode1,
    Mode2,
    Mode3,
}

/// Number of data bits per UART character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UartDataBits {
    Bits5 = 5,
    Bits6 = 6,
    Bits7 = 7,
    Bits8 = 8,
}

/// UART parity setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UartParity {
    None,
    Even,
    Odd,
}

/// Number of stop bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UartStopBits {
    /// 1 stop bit.
    Stop1 = 1,
    /// 1.5 stop bits (encoded as 15 for the register field).
    Stop1_5 = 15,
    /// 2 stop bits.
    Stop2 = 2,
}

// ── Layer 2: Bus abstraction trait ─────────────────────────────────────────

/// Implemented by bus abstractions (`spi_bus`, `i2c_bus`, `uart_bus`).
///
/// These sit above the physical driver and provide a protocol-aware
/// interface.  They are linked directly into the calling task — no IPC.
pub trait Bus: Send + Sync {
    /// Full-duplex transfer. Sends `tx` bytes and receives into `rx`.
    fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()>;

    /// Write-only.  May be more efficient than `transfer` when no response
    /// is expected.
    fn write(&self, data: &[u8]) -> BusResult<()>;

    /// Read-only.  Typically used after writing a register address.
    fn read(&self, buf: &mut [u8]) -> BusResult<()>;

    /// Set the bus clock speed.  Not all buses support dynamic re-clocking.
    fn set_speed(&self, speed: BusSpeed) -> BusResult<()>;

    /// Assert chip-select or start of transaction.
    fn select(&self) -> BusResult<()>;

    /// De-assert chip-select or end of transaction.
    fn deselect(&self) -> BusResult<()>;
}

// ── Layer 1: Physical bus trait ────────────────────────────────────────────

/// Implemented by physical driver crates (e.g. `esp32_spi`).
///
/// These have direct hardware register access and run at the lowest
/// level of the driver stack.
pub trait PhysicalBus: Send + Sync {
    /// Initialise the hardware peripheral with the given configuration.
    fn init(&mut self, config: &BusConfig) -> BusResult<()>;

    /// Perform a raw duplex hardware transfer.
    fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()>;

    /// Enable or disable the peripheral clock.
    fn set_enabled(&mut self, enabled: bool);
}

// ── Bus handle (layer 2 → layer 3 bridge) ──────────────────────────────────

/// Opaque handle returned to a logical driver.
///
/// Internally wraps a `&dyn Bus` reference and forwards all calls via
/// delegation methods.  Logical drivers never see the bus type.
#[derive(Clone)]
pub struct BusHandle {
    pub(crate) inner: &'static dyn Bus,
}

impl BusHandle {
    /// Wrap a bus reference into a handle.
    pub fn new(bus: &'static dyn Bus) -> Self {
        Self { inner: bus }
    }

    /// Full-duplex transfer.
    pub fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        self.inner.transfer(tx, rx)
    }

    /// Write-only.
    pub fn write(&self, data: &[u8]) -> BusResult<()> {
        self.inner.write(data)
    }

    /// Read-only.
    pub fn read(&self, buf: &mut [u8]) -> BusResult<()> {
        self.inner.read(buf)
    }

    /// Assert chip-select / begin transaction.
    pub fn select(&self) -> BusResult<()> {
        self.inner.select()
    }

    /// De-assert chip-select / end transaction.
    pub fn deselect(&self) -> BusResult<()> {
        self.inner.deselect()
    }

    /// Set bus speed.
    pub fn set_speed(&self, speed: BusSpeed) -> BusResult<()> {
        self.inner.set_speed(speed)
    }
}

// ── Board manifest types ───────────────────────────────────────────────────

/// Bus mapping entry in the board manifest.
pub struct BusMapping {
    pub name: &'static str,
    pub kind: BusKind,
    pub base_addr: u32,
    pub irq: u8,
    pub dma_capable: bool,
    pub dma_pool_bytes: u32,
    pub config: BusConfig,
}

/// Kind of bus peripheral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusKind {
    Spi,
    I2c,
    Uart,
    Can,
    OneWire,
}

/// Logical device attached to a bus, declared in the board manifest.
pub struct BusDevice {
    pub name: &'static str,
    pub logical_driver: &'static str,
    pub bus: &'static str,
    pub cs_pin: Option<u8>,
    pub bus_speed: BusSpeed,
}

/// Direct (non-bus-attached) peripheral mapping.
pub struct PeripheralMapping {
    pub name: &'static str,
    pub base_addr: u32,
    pub irq: u8,
    pub dma_capable: bool,
    pub dma_pool_bytes: u32,
}

/// Service task declaration in the board manifest.
pub struct ServiceMapping {
    pub name: &'static str,
    pub always: bool,
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn bus_speed_khz() {
        assert_eq!(BusSpeed::KHz(100).hz(), 100_000);
    }

    #[test]
    fn bus_speed_mhz() {
        assert_eq!(BusSpeed::MHz(40).hz(), 40_000_000);
    }

    #[test]
    fn bus_speed_named() {
        assert_eq!(BusSpeed::Standard100k.hz(), 100_000);
        assert_eq!(BusSpeed::Fast400k.hz(), 400_000);
        assert_eq!(BusSpeed::FastPlus1M.hz(), 1_000_000);
        assert_eq!(BusSpeed::HighSpeed3M4.hz(), 3_400_000);
    }

    #[test]
    fn bus_error_display() {
        assert_eq!(std::format!("{}", BusError::Timeout), "Timeout");
        assert_eq!(std::format!("{}", BusError::DeviceNotResponding), "DeviceNotResponding");
    }

    struct MockBus;

    impl Bus for MockBus {
        fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            let _ = (tx, rx);
            Ok(())
        }
        fn write(&self, _data: &[u8]) -> BusResult<()> { Ok(()) }
        fn read(&self, _buf: &mut [u8]) -> BusResult<()> { Ok(()) }
        fn set_speed(&self, _speed: BusSpeed) -> BusResult<()> { Err(BusError::InvalidConfig) }
        fn select(&self) -> BusResult<()> { Ok(()) }
        fn deselect(&self) -> BusResult<()> { Ok(()) }
    }

    #[test]
    fn bus_handle_delegation() {
        let handle = BusHandle::new(&MockBus);
        let mut rx = [0u8; 4];
        assert!(handle.transfer(b"test", &mut rx).is_ok());
        assert!(handle.write(b"test").is_ok());
        assert!(handle.read(&mut rx).is_ok());
        assert!(handle.select().is_ok());
        assert!(handle.deselect().is_ok());
        assert_eq!(handle.set_speed(BusSpeed::MHz(10)), Err(BusError::InvalidConfig));
    }
}