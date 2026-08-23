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

// ── Coarse delay ─────────────────────────────────────────────────────────────

/// Busy-wait approximately `us` microseconds, for [`Op::delay_us`].
///
/// A stopgap, exactly like `soc_esp32::poll`: the loop bound is iterations, not
/// time, so its real duration drifts with the CPU clock. It exists so a per-op
/// delay means *something* today; a faithful microsecond needs the portable
/// cycle counter (`doc/plan-arm32.md`, Phase 2.4). Conservative by design — at
/// a lower clock it over-waits rather than under-waits.
pub fn spin_rough_us(us: u32) {
    // A floor near 240 MHz at a few cycles per iteration; slower clocks simply
    // wait longer, which is the safe direction for a settling delay.
    for _ in 0..us.saturating_mul(240) {
        core::hint::spin_loop();
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
///
/// One variant per bus kind, each wrapping a named config struct so a driver
/// or port type can hold exactly the one it needs ([`SpiConfig`] on a SPI
/// port, never a UART's). The `const fn` helpers cover the common shapes
/// (`spi_mode0`, `i2c`, `uart_8n1`); anything else is built with struct
/// syntax and `..Default::default()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusConfig {
    /// SPI bus configuration.
    Spi(SpiConfig),
    /// I2C bus configuration.
    I2c(I2cConfig),
    /// UART bus configuration.
    Uart(UartConfig),
}

impl BusConfig {
    /// A mode-0 SPI bus (clock idle low, sample on the rising edge).
    pub const fn spi_mode0(mosi: u8, miso: u8, sck: u8, max_speed: BusSpeed) -> Self {
        BusConfig::Spi(SpiConfig { mosi, miso, sck, max_speed, mode: SpiMode::Mode0 })
    }

    /// An I2C bus at `speed`.
    pub const fn i2c(sda: u8, scl: u8, speed: BusSpeed) -> Self {
        BusConfig::I2c(I2cConfig { sda, scl, speed })
    }

    /// A UART framed 8 data bits, no parity, 1 stop bit — the console default.
    pub const fn uart_8n1(tx: u8, rx: u8, baud: u32) -> Self {
        BusConfig::Uart(UartConfig {
            tx,
            rx,
            baud,
            data_bits: UartDataBits::Bits8,
            parity: UartParity::None,
            stop_bits: UartStopBits::Stop1,
        })
    }
}

/// Pins, clock ceiling and mode for a SPI controller.
///
/// `Default` is mode 0 at 1 MHz on pins 0/0/0 — a placeholder to spread with
/// `..`, not a wiring anyone should run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpiConfig {
    pub mosi: u8,
    pub miso: u8,
    pub sck: u8,
    pub max_speed: BusSpeed,
    pub mode: SpiMode,
}

impl Default for SpiConfig {
    fn default() -> Self {
        Self { mosi: 0, miso: 0, sck: 0, max_speed: BusSpeed::MHz(1), mode: SpiMode::Mode0 }
    }
}

/// Pins and clock for an I2C controller. `Default` is standard-mode (100 kHz)
/// on pins 0/0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct I2cConfig {
    pub sda: u8,
    pub scl: u8,
    pub speed: BusSpeed,
}

impl Default for I2cConfig {
    fn default() -> Self {
        Self { sda: 0, scl: 0, speed: BusSpeed::Standard100k }
    }
}

/// Pins, baud rate and framing for a UART. `Default` is 115200 8N1 on pins
/// 0/0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartConfig {
    pub tx: u8,
    pub rx: u8,
    pub baud: u32,
    pub data_bits: UartDataBits,
    pub parity: UartParity,
    pub stop_bits: UartStopBits,
}

impl Default for UartConfig {
    fn default() -> Self {
        Self {
            tx: 0,
            rx: 0,
            baud: 115_200,
            data_bits: UartDataBits::Bits8,
            parity: UartParity::None,
            stop_bits: UartStopBits::Stop1,
        }
    }
}

/// SPI clock polarity / phase modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpiMode {
    #[default]
    Mode0,
    Mode1,
    Mode2,
    Mode3,
}

/// Number of data bits per UART character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UartDataBits {
    Bits5 = 5,
    Bits6 = 6,
    Bits7 = 7,
    #[default]
    Bits8 = 8,
}

/// UART parity setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UartParity {
    #[default]
    None,
    Even,
    Odd,
}

/// Number of stop bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UartStopBits {
    /// 1 stop bit.
    #[default]
    Stop1 = 1,
    /// 1.5 stop bits (encoded as 15 for the register field).
    Stop1_5 = 15,
    /// 2 stop bits.
    Stop2 = 2,
}

// ── Transfer operations ─────────────────────────────────────────────────────

/// Chip-select behaviour after an [`Op`] completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CsHold {
    /// Release chip-select — this op ends its transaction (the common case).
    #[default]
    Release,
    /// Keep chip-select asserted for the next op, so several ops form one
    /// transaction (e.g. write a register address, then read the value).
    Keep,
}

/// One step of a bus transfer list.
///
/// [`Bus::transfer`] takes a slice of these and runs them in order. Both `tx`
/// and `rx` set is a full-duplex exchange; `tx` alone is a write; `rx` alone is
/// a read. The struct is `#[non_exhaustive]`: build it with [`Op::write`],
/// [`Op::read`], or [`Op::exchange`] and the builder setters, so later fields
/// (per-op speed, and so on) stay additive.
#[non_exhaustive]
pub struct Op<'a> {
    /// Bytes to send, if any.
    pub tx: Option<&'a [u8]>,
    /// Buffer to receive into, if any.
    pub rx: Option<&'a mut [u8]>,
    /// Bits per word. 8 unless the bus and driver support wider words.
    pub word_bits: u8,
    /// Chip-select handling once this op finishes.
    pub cs: CsHold,
    /// Microseconds to idle after this op, before the next one runs.
    pub delay_us: u32,
}

impl<'a> Op<'a> {
    /// A write of `tx`.
    pub fn write(tx: &'a [u8]) -> Self {
        Self { tx: Some(tx), rx: None, word_bits: 8, cs: CsHold::Release, delay_us: 0 }
    }

    /// A read into `rx`.
    pub fn read(rx: &'a mut [u8]) -> Self {
        Self { tx: None, rx: Some(rx), word_bits: 8, cs: CsHold::Release, delay_us: 0 }
    }

    /// A simultaneous full-duplex exchange: send `tx` while receiving `rx`.
    pub fn exchange(tx: &'a [u8], rx: &'a mut [u8]) -> Self {
        Self { tx: Some(tx), rx: Some(rx), word_bits: 8, cs: CsHold::Release, delay_us: 0 }
    }

    /// Set the word width, in bits.
    pub fn with_word_bits(mut self, bits: u8) -> Self {
        self.word_bits = bits;
        self
    }

    /// Keep chip-select asserted after this op (part of a larger transaction).
    pub fn keep_cs(mut self) -> Self {
        self.cs = CsHold::Keep;
        self
    }

    /// Idle `us` microseconds after this op.
    pub fn then_delay_us(mut self, us: u32) -> Self {
        self.delay_us = us;
        self
    }
}

// ── Layer 2: Bus abstraction trait ─────────────────────────────────────────

/// Implemented by bus abstractions (`spi_bus`, `i2c_bus`).
///
/// UART is deliberately absent: it is a byte stream, not an addressed bus —
/// see [`crate::stream`].
///
/// These sit above the physical driver and provide a protocol-aware
/// interface.  They are linked directly into the calling task — no IPC.
///
/// The single transfer entry point is a *list* of [`Op`]s run as one logical
/// transaction. The old copy-based `write`/`read`/`transfer(tx, rx)` methods
/// live on [`BusHandle`] now, as thin wrappers that build a one- or two-element
/// `[Op]` — so logical drivers keep calling them unchanged.
pub trait Bus: Send + Sync {
    /// Run a list of operations in order, as one logical transaction.
    ///
    /// Each [`Op`] is a write, a read, or a full-duplex exchange; the struct
    /// carries its word width, chip-select hold, and trailing delay. A payload
    /// longer than [`Bus::max_transfer`] is either moved whole in pieces or
    /// refused with an error — it is never silently cut short (#98). A bus
    /// that splits an op may release chip-select between the pieces, so a
    /// device that needs one framed transaction must fit in `max_transfer`.
    fn transfer(&self, ops: &mut [Op]) -> BusResult<()>;

    /// Largest single-op payload, in bytes, this bus moves in one go — the
    /// controller FIFO depth. The bound a device-framed transaction (one
    /// chip-select assertion) has to fit in.
    fn max_transfer(&self) -> usize;

    /// Which wire protocol this bus speaks. [`BusHandle`] dispatches on it:
    /// a register read is one full-duplex exchange on SPI but a write then a
    /// repeated-start read on I2C, and only the handle can tell which it
    /// needs (#97). No default, so a mock cannot quietly claim the wrong one.
    fn kind(&self) -> BusKind;

    /// Set the bus clock / data rate. Default: not dynamically reclockable.
    fn set_speed(&self, _speed: BusSpeed) -> BusResult<()> {
        Err(BusError::InvalidConfig)
    }
}

// ── Layer 1: Physical bus trait ────────────────────────────────────────────

/// The run-time half of a physical driver: everything that moves bytes on a
/// peripheral that is already up. Every method takes `&self`, so a shared
/// reference to a driver is itself a `PhysicalTransfer` (the blanket impl
/// below) and a bus layer can hold one without owning the driver.
///
/// Construction-time work (`init`, `&mut self`) lives on [`PhysicalBus`],
/// which extends this trait; the split is what makes the blanket impl sound.
pub trait PhysicalTransfer: Send + Sync {
    /// Perform a raw full-duplex hardware exchange: clock `tx` out while
    /// filling `rx`.
    ///
    /// **For I2C, `tx[0]` is the device's 7-bit address, unshifted.** The
    /// physical driver adds the R/W bit. This was never written down, and the
    /// two layers ended up disagreeing: the bus layer pre-shifted in one
    /// method and not in another, while the physical driver shifted again, so
    /// `0x76` reached the wire as `0xD8` and nothing would ACK it.
    ///
    /// The three shapes an implementation must handle:
    ///
    /// | `tx` | `rx` | Meaning |
    /// |---|---|---|
    /// | addr + data | empty | write only |
    /// | addr | non-empty | read only |
    /// | addr + data | non-empty | write, repeated start, read |
    ///
    /// `rx` must be filled on return. Returning `Ok(())` having left the
    /// bytes in a FIFO is what the ESP32 I2C driver used to do.
    ///
    /// SPI and UART ignore the addressing rule: `tx` is data, `rx` is the
    /// buffer, and the two are clocked together.
    fn exchange(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()>;

    /// Re-program the bus clock / data rate on a live peripheral, without a
    /// full re-init. Default: unsupported. SPI overrides it.
    fn set_speed(&self, _speed: BusSpeed) -> BusResult<()> {
        Err(BusError::InvalidConfig)
    }
}

/// A shared reference to a transfer-capable driver is one itself. Sound
/// because nothing on [`PhysicalTransfer`] takes `&mut self`.
impl<T: PhysicalTransfer + ?Sized> PhysicalTransfer for &T {
    fn exchange(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        (**self).exchange(tx, rx)
    }

    fn set_speed(&self, speed: BusSpeed) -> BusResult<()> {
        (**self).set_speed(speed)
    }
}

/// Implemented by physical driver crates (e.g. `esp32_spi`).
///
/// These have direct hardware register access and run at the lowest level of
/// the driver stack. This trait adds the one construction-time method to
/// [`PhysicalTransfer`]; once `init` has run, callers need only the supertrait.
pub trait PhysicalBus: PhysicalTransfer {
    /// Initialise the hardware peripheral with the given configuration.
    fn init(&mut self, config: &BusConfig) -> BusResult<()>;
}

// ── Bus handle (layer 2 → layer 3 bridge) ──────────────────────────────────

/// Opaque handle returned to a logical driver.
///
/// Wraps a `&dyn Bus` and offers the small copy-based surface logical drivers
/// were written against — each method builds a `[Op]` and calls
/// [`Bus::transfer`]. Logical drivers never see the bus type, and never had to
/// change when the trait moved to a transfer list.
///
/// The borrow is `'a`, not `'static`: every method here takes `&self` and
/// [`Bus::transfer`] does too, so nothing the handle does needs the bus to
/// live forever. A driver can therefore be handed a bus that lives only as
/// long as the task's stack frame — a board no longer has to leak its buses
/// into `static`s just to satisfy this type. Building one is usually
/// `(&bus).into()` via the [`From`] impl below, so a logical driver's
/// constructor reads `Mpu6886::new(&bus)` rather than
/// `Mpu6886::new(BusHandle::new(&bus))`.
#[derive(Clone, Copy)]
pub struct BusHandle<'a> {
    pub(crate) inner: &'a dyn Bus,
}

/// `(&bus).into()` builds a handle, so a logical driver's `new` can take
/// `impl Into<BusHandle<'a>>` and be called with a plain `&bus`.
impl<'a, B: Bus> From<&'a B> for BusHandle<'a> {
    fn from(bus: &'a B) -> Self {
        Self { inner: bus }
    }
}

/// Longest burst [`BusHandle::read_regs`] reads over SPI. The handle stages
/// the exchange in a stack buffer one byte longer than this; 64 matches the
/// SPI controller's data buffer, so a burst under it is one framed transfer.
pub const REG_BURST_MAX: usize = 64;

impl<'a> BusHandle<'a> {
    /// Wrap a bus reference into a handle.
    pub fn new(bus: &'a dyn Bus) -> Self {
        Self { inner: bus }
    }

    /// Full-duplex exchange: send `tx` while receiving `rx`. On I2C this is a
    /// write of `tx` followed by a repeated-start read into `rx`.
    ///
    /// On SPI the two buffers are clocked together, so only `min(tx, rx)`
    /// bytes move: `transfer(&[reg], &mut six_bytes)` reads one byte, not six.
    /// A register read that has to work on either bus is
    /// [`BusHandle::read_regs`].
    pub fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        self.inner.transfer(&mut [Op::exchange(tx, rx)])
    }

    /// Which wire protocol the bus underneath speaks. See [`Bus::kind`].
    pub fn kind(&self) -> BusKind {
        self.inner.kind()
    }

    /// Write one byte to a register: the address, then the value, in one
    /// transaction. The same bytes on SPI and I2C.
    ///
    /// The address goes out as given. A device that marks a write with a bit
    /// in the address (Bosch parts clear bit 7 over SPI) has the driver apply
    /// it; the handle knows the bus, not the part.
    pub fn write_reg(&self, reg: u8, val: u8) -> BusResult<()> {
        self.inner.transfer(&mut [Op::write(&[reg, val])])
    }

    /// Burst-read `buf.len()` bytes starting at register `reg`, the way any
    /// register-mapped device with an auto-incrementing address does it.
    ///
    /// - SPI: one full-duplex exchange of `1 + buf.len()` bytes — the address
    ///   goes out in the first slot, the device answers in the rest — and the
    ///   reply is copied into `buf`. One exchange, not a write op then a read
    ///   op, because the SPI bus may drop chip-select between ops and the
    ///   device would forget the address. At most [`REG_BURST_MAX`] bytes;
    ///   longer is refused with `InvalidConfig`.
    /// - I2C: write the address, repeated-start, read `buf`.
    ///
    /// Until #97 `read_reg` did an SPI exchange of the 1-byte address against
    /// an N-byte buffer, which clocks `min(1, N)` = one byte on SPI and was
    /// only right on I2C.
    ///
    /// The address goes out as given; a read-marker bit (bit 7 on Bosch and
    /// InvenSense parts over SPI) is the driver's to set.
    pub fn read_regs(&self, reg: u8, buf: &mut [u8]) -> BusResult<()> {
        match self.inner.kind() {
            BusKind::I2c => self.inner.transfer(&mut [Op::exchange(&[reg], buf)]),
            BusKind::Spi => {
                if buf.len() > REG_BURST_MAX {
                    return Err(BusError::InvalidConfig);
                }
                let n = 1 + buf.len();
                let mut tx = [0u8; 1 + REG_BURST_MAX];
                let mut rx = [0u8; 1 + REG_BURST_MAX];
                tx[0] = reg;
                self.inner.transfer(&mut [Op::exchange(&tx[..n], &mut rx[..n])])?;
                buf.copy_from_slice(&rx[1..n]);
                Ok(())
            }
            // Not a register-mapped bus.
            _ => Err(BusError::InvalidConfig),
        }
    }

    /// Write `tx`, then read `rx` — two ops with chip-select held across them,
    /// for a device that answers a command on a later clock (typical SPI).
    pub fn write_read(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        self.inner.transfer(&mut [Op::write(tx).keep_cs(), Op::read(rx)])
    }

    /// Write-only.
    pub fn write(&self, data: &[u8]) -> BusResult<()> {
        self.inner.transfer(&mut [Op::write(data)])
    }

    /// Read-only.
    pub fn read(&self, buf: &mut [u8]) -> BusResult<()> {
        self.inner.transfer(&mut [Op::read(buf)])
    }

    /// Read one register. [`BusHandle::read_regs`] for one byte.
    pub fn read_reg(&self, reg: u8) -> BusResult<u8> {
        let mut buf = [0u8; 1];
        self.read_regs(reg, &mut buf)?;
        Ok(buf[0])
    }

    /// Set bus speed.
    pub fn set_speed(&self, speed: BusSpeed) -> BusResult<()> {
        self.inner.set_speed(speed)
    }

    /// Largest single-op payload this bus moves in one go. See
    /// [`Bus::max_transfer`].
    pub fn max_transfer(&self) -> usize {
        self.inner.max_transfer()
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

    use core::sync::atomic::{AtomicUsize, Ordering};

    /// Echoes each op's `tx` into its `rx`, and records how many ops it saw.
    struct MockBus {
        ops_seen: AtomicUsize,
    }

    impl Bus for MockBus {
        fn transfer(&self, ops: &mut [Op]) -> BusResult<()> {
            self.ops_seen.store(ops.len(), Ordering::Relaxed);
            for op in ops.iter_mut() {
                if let (Some(tx), Some(rx)) = (op.tx, op.rx.as_deref_mut()) {
                    let n = tx.len().min(rx.len());
                    rx[..n].copy_from_slice(&tx[..n]);
                }
            }
            Ok(())
        }
        fn max_transfer(&self) -> usize {
            64
        }
        fn kind(&self) -> BusKind {
            BusKind::Spi
        }
    }

    /// A register-mapped device on a bus of the given kind, modelled at the
    /// wire: on SPI every byte clocked out is answered by one clocked in, so
    /// an exchange moves `min(tx, rx)` bytes and the first reply byte is junk;
    /// on I2C a write of the address then a read returns the bytes from that
    /// address on. Either way register `r` holds `r + 1`, and auto-increments.
    struct RegDevice {
        kind: BusKind,
        writes: Mutex<Vec<(u8, u8)>>,
    }

    use std::sync::Mutex;
    use std::vec::Vec;

    impl Bus for RegDevice {
        fn transfer(&self, ops: &mut [Op]) -> BusResult<()> {
            for op in ops.iter_mut() {
                match (self.kind, op.tx, op.rx.as_deref_mut()) {
                    (BusKind::Spi, Some(tx), Some(rx)) => {
                        let n = tx.len().min(rx.len());
                        rx[0] = 0xFF; // whatever was on MISO during the address
                        for (i, b) in rx[1..n].iter_mut().enumerate() {
                            *b = tx[0].wrapping_add(1 + i as u8);
                        }
                    }
                    (BusKind::I2c, Some(tx), Some(rx)) => {
                        for (i, b) in rx.iter_mut().enumerate() {
                            *b = tx[0].wrapping_add(1 + i as u8);
                        }
                    }
                    (_, Some(tx), None) if tx.len() == 2 => {
                        self.writes.lock().unwrap().push((tx[0], tx[1]));
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        fn max_transfer(&self) -> usize {
            64
        }
        fn kind(&self) -> BusKind {
            self.kind
        }
    }

    fn reg_device(kind: BusKind) -> (BusHandle<'static>, &'static RegDevice) {
        let dev: &'static RegDevice =
            std::boxed::Box::leak(std::boxed::Box::new(RegDevice { kind, writes: Mutex::new(Vec::new()) }));
        (BusHandle::new(dev), dev)
    }

    #[test]
    fn read_regs_bursts_the_right_bytes_on_both_kinds() {
        // Regression guard (#97): over SPI this used to exchange a 1-byte
        // address against the N-byte buffer, which clocks one byte.
        for kind in [BusKind::Spi, BusKind::I2c] {
            let (h, _) = reg_device(kind);
            let mut six = [0u8; 6];
            h.read_regs(0x10, &mut six).unwrap();
            assert_eq!(six, [0x11, 0x12, 0x13, 0x14, 0x15, 0x16], "{kind:?}");
            assert_eq!(h.read_reg(0x75).unwrap(), 0x76, "{kind:?}");
            assert_eq!(h.kind(), kind);
        }
    }

    #[test]
    fn write_reg_sends_address_then_value_on_both_kinds() {
        for kind in [BusKind::Spi, BusKind::I2c] {
            let (h, dev) = reg_device(kind);
            h.write_reg(0xF4, 0x27).unwrap();
            assert_eq!(&dev.writes.lock().unwrap()[..], &[(0xF4, 0x27)], "{kind:?}");
        }
    }

    #[test]
    fn an_spi_burst_past_the_staging_buffer_is_refused() {
        let (h, _) = reg_device(BusKind::Spi);
        let mut big = [0u8; REG_BURST_MAX + 1];
        assert_eq!(h.read_regs(0, &mut big), Err(BusError::InvalidConfig));
        let mut max = [0u8; REG_BURST_MAX];
        h.read_regs(0, &mut max).unwrap();
        assert_eq!(max[REG_BURST_MAX - 1], REG_BURST_MAX as u8);
    }

    #[test]
    fn bus_handle_delegation() {
        let bus = MockBus { ops_seen: AtomicUsize::new(0) };
        // No lifetime gymnastics: the handle borrows `bus` for the block.
        let handle = BusHandle::new(&bus);
        let mut rx = [0u8; 4];

        assert!(handle.transfer(b"test", &mut rx).is_ok());
        assert_eq!(&rx, b"test"); // exchange echoes tx into rx
        assert_eq!(bus.ops_seen.load(Ordering::Relaxed), 1);

        assert!(handle.write(b"test").is_ok());
        assert_eq!(bus.ops_seen.load(Ordering::Relaxed), 1);

        assert!(handle.read(&mut rx).is_ok());
        assert_eq!(bus.ops_seen.load(Ordering::Relaxed), 1);

        assert!(handle.write_read(b"ab", &mut rx).is_ok());
        assert_eq!(bus.ops_seen.load(Ordering::Relaxed), 2); // write op + read op

        assert_eq!(handle.set_speed(BusSpeed::MHz(10)), Err(BusError::InvalidConfig));
        assert_eq!(handle.max_transfer(), 64);
    }

    #[test]
    fn bus_config_helpers_fill_the_fixed_fields() {
        let BusConfig::Uart(u) = BusConfig::uart_8n1(1, 3, 9600) else { panic!() };
        assert_eq!((u.tx, u.rx, u.baud), (1, 3, 9600));
        assert_eq!(u.data_bits, UartDataBits::Bits8);
        assert_eq!(u.parity, UartParity::None);
        assert_eq!(u.stop_bits, UartStopBits::Stop1);

        let BusConfig::Spi(s) = BusConfig::spi_mode0(23, 19, 18, BusSpeed::MHz(10)) else {
            panic!()
        };
        assert_eq!((s.mosi, s.miso, s.sck), (23, 19, 18));
        assert_eq!(s.max_speed, BusSpeed::MHz(10));
        assert_eq!(s.mode, SpiMode::Mode0);

        let BusConfig::I2c(i) = BusConfig::i2c(21, 22, BusSpeed::Fast400k) else { panic!() };
        assert_eq!((i.sda, i.scl, i.speed), (21, 22, BusSpeed::Fast400k));

        // Defaults agree with the 8N1 helper on framing.
        let d = UartConfig::default();
        assert_eq!((d.data_bits, d.parity, d.stop_bits), (u.data_bits, u.parity, u.stop_bits));
    }

    /// Echoes `tx` into `rx`; exists to prove a `&T` is a `PhysicalTransfer`.
    struct Echo;

    impl PhysicalTransfer for Echo {
        fn exchange(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            let n = tx.len().min(rx.len());
            rx[..n].copy_from_slice(&tx[..n]);
            Ok(())
        }
    }

    #[test]
    fn shared_reference_is_a_physical_transfer() {
        fn run<P: PhysicalTransfer>(p: P) -> BusResult<[u8; 2]> {
            let mut rx = [0u8; 2];
            p.exchange(&[7, 9], &mut rx)?;
            Ok(rx)
        }
        let echo = Echo;
        assert_eq!(run(&echo), Ok([7, 9]));
        let dynamic: &dyn PhysicalTransfer = &echo;
        assert_eq!(run(dynamic), Ok([7, 9]));
        // The `&T` blanket forwards the default `set_speed` too.
        let by_ref: &Echo = &echo;
        assert_eq!(PhysicalTransfer::set_speed(&by_ref, BusSpeed::MHz(1)), Err(BusError::InvalidConfig));
    }

    #[test]
    fn a_handle_borrows_a_stack_bus_via_from() {
        // The ergonomic win of the `'a` handle (#115): a bus that lives only
        // in this stack frame, no `'static`, no transmute, and `(&bus).into()`
        // so a driver's `new` needs no `BusHandle::new` at the call site.
        let bus = MockBus { ops_seen: AtomicUsize::new(0) };
        let handle: BusHandle = (&bus).into();
        let mut rx = [0u8; 4];
        assert!(handle.transfer(b"test", &mut rx).is_ok());
        assert_eq!(&rx, b"test");
    }

    #[test]
    fn op_builders_set_their_fields() {
        let data = [1u8, 2, 3];
        let op = Op::write(&data).with_word_bits(16).keep_cs().then_delay_us(5);
        assert_eq!(op.tx, Some(&data[..]));
        assert!(op.rx.is_none());
        assert_eq!(op.word_bits, 16);
        assert_eq!(op.cs, CsHold::Keep);
        assert_eq!(op.delay_us, 5);
    }
}