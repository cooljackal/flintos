// SPDX-License-Identifier: Apache-2.0

//! The contract between a display *panel* driver and the *interface* that moves
//! its bytes.
//!
//! A MIPI-style display (ILI9342C, ILI9341, ST7789, …) is driven as a stream of
//! command bytes and pixel data over some transport — SPI with a data/command
//! line, an 8/16-bit parallel bus, whatever. A panel driver knows the command
//! set and the pixel format; it does not know, and should not care, whether the
//! bytes leave over polled SPI, DMA, or a parallel bus. [`DisplayInterface`] is
//! that seam: the panel driver (Layer 3, portable) calls it, and a concrete
//! implementation (built by the board from its SPI + GPIO + DMA) makes the bytes
//! move as fast as the hardware allows.
//!
//! # Why the transaction shape is in the trait
//!
//! The single biggest determinant of display throughput is *not* the clock — it
//! is how many SPI transactions a screen update costs. A naive driver toggles
//! chip-select around every command and every pixel run; a fast one asserts CS
//! once, streams a whole windowed fill, and deasserts. So the trait is built
//! around an explicit transaction ([`start`](DisplayInterface::start) /
//! [`end`](DisplayInterface::end)) with the command, data and pixel calls in
//! between running under one held CS — the same shape TFT_eSPI's
//! `startWrite`/`endWrite` and LovyanGFX's bus lock use, for the same reason.
//!
//! # Pixel format
//!
//! Pixels are 16-bit RGB565. The interface owns the on-the-wire byte order
//! (MIPI displays take the high byte first), so a panel driver hands it native
//! `u16` values and never byte-swaps itself — which lets the interface fold the
//! swap into the same copy that fills its DMA buffer, at no extra cost.

/// Why a display operation failed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DisplayError {
    /// The underlying transport (SPI, DMA, GPIO) failed.
    Interface,
    /// A draw fell outside the panel's bounds.
    OutOfBounds,
    /// A pixel buffer did not match the region it was meant to fill.
    SizeMismatch,
}

/// The transport a panel driver streams command and pixel bytes through.
///
/// Every method between a [`start`](Self::start) and an [`end`](Self::end) runs
/// with chip-select held, so a windowed fill or blit is one SPI transaction.
/// Implementations own chip-select, the data/command line, byte order, and any
/// DMA — see the module docs.
pub trait DisplayInterface {
    /// Begin a transaction: assert chip-select. Commands, data and pixels sent
    /// before the matching [`end`](Self::end) share one held CS.
    fn start(&mut self) -> Result<(), DisplayError>;

    /// End the transaction: deassert chip-select.
    fn end(&mut self) -> Result<(), DisplayError>;

    /// Send one command byte, with the data/command line low.
    fn command(&mut self, cmd: u8) -> Result<(), DisplayError>;

    /// Send command arguments or small data, with the data/command line high.
    /// For a handful of bytes (a window address, a register value); pixel runs
    /// go through [`fill`](Self::fill) / [`pixels`](Self::pixels).
    fn data(&mut self, bytes: &[u8]) -> Result<(), DisplayError>;

    /// Stream `count` copies of one RGB565 pixel — a solid fill — as pixel data.
    ///
    /// The interface packs the color once and streams it by DMA, so a full-screen
    /// clear costs no per-pixel CPU work.
    fn fill(&mut self, color: u16, count: usize) -> Result<(), DisplayError>;

    /// Stream a run of RGB565 pixels — a blit — as pixel data.
    ///
    /// The interface converts to wire order and streams by DMA, overlapping the
    /// conversion of the next chunk with the transfer of the current one.
    fn pixels(&mut self, pixels: &[u16]) -> Result<(), DisplayError>;
}
