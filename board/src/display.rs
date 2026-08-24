// SPDX-License-Identifier: Apache-2.0

//! A DMA-backed [`DisplayInterface`] for SPI TFT panels, built from the board's
//! SPI controller and two GPIOs.
//!
//! This is the concrete transport the ILI9342C driver streams through — the one
//! place that composes the SPI controller (Layer 1), the GPIO driver (the D/C
//! and chip-select lines) and the DMA pool. It lives in the board crate because
//! that is the only tier that names more than one physical driver, the same
//! reason the console and the I2C accessors do.
//!
//! # How it goes fast
//!
//! - **One transaction per fill/blit.** Chip-select is a GPIO the interface
//!   holds low from [`start`](DisplayInterface::start) to [`end`], so a windowed
//!   fill is a single SPI transaction rather than one per byte.
//! - **DMA for pixels, FIFO for commands.** Commands and the handful of window
//!   bytes go out the 64-byte FIFO (no DMA setup cost); pixel runs go by DMA.
//! - **Double-buffered blits.** [`pixels`](DisplayInterface::pixels) converts the
//!   next chunk into one DMA buffer while the previous chunk transfers out of the
//!   other, so the byte-order conversion overlaps the transfer.
//!
//! # The interrupt the caller must wire
//!
//! DMA completion is reported by the SPI controller's interrupt. The top-half
//! that acknowledges it and wakes the transfer names the kernel's DMA broker,
//! which neither this crate nor an ordinary application may — so the caller
//! connects it once (see [`crate::lcd_spi`] for the shared controller the
//! handler acknowledges). Without it, a pixel transfer times out.

use api::display::{DisplayError, DisplayInterface};
use api::dma::{self, DmaHandle};
use esp32_gpio::{Esp32Gpio, PinLevel, PinMode};
use esp32_spi::Esp32Spi;

/// Most a single DMA chunk is allowed to grow to. Throughput is DMA-clock-
/// bound, so a larger chunk than this buys almost nothing — it only reduces the
/// count of per-transaction gaps, which flattens out quickly. Capping the
/// appetite here means a board with a large pool leaves the rest of it for
/// other DMA users rather than the display swallowing all of it.
const MAX_CHUNK_BYTES: u32 = 4096;
/// Smallest chunk worth running. Below this the per-transaction overhead
/// dominates; if the pool cannot spare three of these the allocation fails and
/// the caller hears about it rather than limping.
const MIN_CHUNK_BYTES: u32 = 1024;
/// The FIFO's limit; commands and window data stay under it.
const FIFO_MAX: usize = 64;

/// A SPI + D/C + CS display transport with DMA pixel streaming.
pub struct Esp32DisplayInterface {
    spi: &'static Esp32Spi,
    gpio: &'static Esp32Gpio,
    dc: u8,
    cs: u8,
    /// Double buffer for pixel DMA.
    tx: [DmaHandle; 2],
    /// Throwaway receive buffer (the engine is full-duplex; MISO is ignored).
    rx: DmaHandle,
    /// RGB565 pixels one chunk holds — half the allocated buffer size. Chosen
    /// from the pool at construction, not a constant, so the display uses
    /// whatever DMA the build's memory map left it (#140).
    chunk_px: usize,
}

impl Esp32DisplayInterface {
    /// Build the interface over a shared SPI controller and the panel's D/C and
    /// chip-select GPIOs. Allocates the DMA buffers from the pool, so it must be
    /// called from the task that will draw (the broker ties a buffer to its
    /// allocating task).
    pub fn new(spi: &'static Esp32Spi, dc: u8, cs: u8) -> hal::Result<Self> {
        let gpio = Esp32Gpio::instance();
        gpio.set_mode(dc, PinMode::Output)?;
        gpio.set_mode(cs, PinMode::Output)?;
        gpio.write(cs, PinLevel::High)?; // idle high
        // Size the chunk from what the pool can spare across the three buffers
        // (double tx + one throwaway rx), rounded to whole RGB565 pixels and
        // held between the min-worth-it and the point of diminishing returns.
        let chunk = (dma::available() / 3 & !1).clamp(MIN_CHUNK_BYTES, MAX_CHUNK_BYTES);
        let tx = [dma::alloc(chunk)?, dma::alloc(chunk)?];
        let rx = dma::alloc(chunk)?;
        Ok(Self { spi, gpio, dc, cs, tx, rx, chunk_px: (chunk / 2) as usize })
    }

    /// Pack `n` copies of one RGB565 color, big-endian (the wire order), into a
    /// DMA buffer.
    ///
    /// # Safety
    /// `2*n <= buf.size()`, i.e. `n <= self.chunk_px` for the interface's own
    /// buffers.
    unsafe fn pack_solid(buf: &DmaHandle, color: u16, n: usize) {
        let (hi, lo) = ((color >> 8) as u8, color as u8);
        let p = buf.addr() as *mut u8;
        for i in 0..n {
            p.add(2 * i).write_volatile(hi);
            p.add(2 * i + 1).write_volatile(lo);
        }
    }

    /// Pack a run of RGB565 pixels, big-endian, into a DMA buffer.
    ///
    /// # Safety
    /// `px.len() <= self.chunk_px`, i.e. `2*px.len() <= buf.size()`.
    unsafe fn pack_pixels(buf: &DmaHandle, px: &[u16]) {
        let p = buf.addr() as *mut u8;
        for (i, &c) in px.iter().enumerate() {
            p.add(2 * i).write_volatile((c >> 8) as u8);
            p.add(2 * i + 1).write_volatile(c as u8);
        }
    }

    /// A small write over the FIFO (commands, window bytes). `bytes.len()` must
    /// be `<= FIFO_MAX`.
    fn fifo(&self, bytes: &[u8]) -> Result<(), DisplayError> {
        let mut sink = [0u8; FIFO_MAX];
        self.spi
            .fifo_exchange(bytes, &mut sink[..bytes.len()])
            .map_err(|_| DisplayError::Interface)
    }

    fn set(&self, pin: u8, level: PinLevel) -> Result<(), DisplayError> {
        self.gpio.write(pin, level).map_err(|_| DisplayError::Interface)
    }

    /// DMA one packed buffer of `bytes` and block until it completes.
    fn dma_send(&self, buf: usize, bytes: usize) -> Result<(), DisplayError> {
        let xfer = self
            .spi
            .exchange_async(&self.tx[buf], &self.rx, bytes)
            .map_err(|_| DisplayError::Interface)?;
        xfer.await_done().map_err(|_| DisplayError::Interface)
    }
}

impl DisplayInterface for Esp32DisplayInterface {
    fn start(&mut self) -> Result<(), DisplayError> {
        self.set(self.cs, PinLevel::Low)
    }

    fn end(&mut self) -> Result<(), DisplayError> {
        self.set(self.cs, PinLevel::High)
    }

    fn command(&mut self, cmd: u8) -> Result<(), DisplayError> {
        self.set(self.dc, PinLevel::Low)?;
        self.fifo(&[cmd])
    }

    fn data(&mut self, bytes: &[u8]) -> Result<(), DisplayError> {
        self.set(self.dc, PinLevel::High)?;
        for chunk in bytes.chunks(FIFO_MAX) {
            self.fifo(chunk)?;
        }
        Ok(())
    }

    fn fill(&mut self, color: u16, count: usize) -> Result<(), DisplayError> {
        if count == 0 {
            return Ok(());
        }
        self.set(self.dc, PinLevel::High)?;
        // Pack once: every chunk sends the same color, so buffer 0 is filled to
        // its cap and reused. A short final chunk just sends a prefix of it.
        let packed = count.min(self.chunk_px);
        // SAFETY: packed <= self.chunk_px.
        unsafe { Self::pack_solid(&self.tx[0], color, packed) };
        let mut remaining = count;
        while remaining > 0 {
            let n = remaining.min(self.chunk_px);
            self.dma_send(0, n * 2)?;
            remaining -= n;
        }
        Ok(())
    }

    fn pixels(&mut self, px: &[u16]) -> Result<(), DisplayError> {
        if px.is_empty() {
            return Ok(());
        }
        self.set(self.dc, PinLevel::High)?;
        // Double-buffered: convert chunk i+1 into the spare buffer while chunk i
        // is transferring out of the other one.
        let total = px.len();
        let n0 = total.min(self.chunk_px);
        // SAFETY: n0 <= self.chunk_px.
        unsafe { Self::pack_pixels(&self.tx[0], &px[..n0]) };
        let mut xfer = self
            .spi
            .exchange_async(&self.tx[0], &self.rx, n0 * 2)
            .map_err(|_| DisplayError::Interface)?;
        let mut off = n0;
        let mut buf = 1usize;
        while off < total {
            let n = (total - off).min(self.chunk_px);
            // SAFETY: n <= self.chunk_px; tx[buf] is the buffer not currently in
            // flight (buf alternates and each transfer is awaited before the
            // next kick), so writing it does not race the DMA.
            unsafe { Self::pack_pixels(&self.tx[buf], &px[off..off + n]) };
            xfer.await_done().map_err(|_| DisplayError::Interface)?;
            xfer = self
                .spi
                .exchange_async(&self.tx[buf], &self.rx, n * 2)
                .map_err(|_| DisplayError::Interface)?;
            off += n;
            buf ^= 1;
        }
        xfer.await_done().map_err(|_| DisplayError::Interface)
    }
}
