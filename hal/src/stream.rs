// SPDX-License-Identifier: Apache-2.0

//! Byte streams — the subsystem a UART actually is.
//!
//! # Bus or subsystem?
//!
//! [`Bus`](crate::bus::Bus) models an **addressed transaction**: a master names
//! a device, moves a bounded payload, and the exchange completes. SPI and I²C
//! fit — there is an address (or a chip-select), the bytes out and in are one
//! transfer, and when it returns it is done.
//!
//! A UART is none of that. Bytes **flow**: there is no address, no chip-select,
//! and what arrives is unrelated to what was sent (in a loopback it happens to
//! match; on a real link it never does). Modelling it as a `Bus` forces a
//! `transfer(tx, rx)` whose "rx" is a fiction — which is exactly the pretense
//! this module retires.
//!
//! The rule, written down so the next peripheral does not have to relitigate it:
//!
//! > **If a transfer has an address and completes, it is a `Bus`. If it flows,
//! > or is locked to a clock, it is a subsystem with its own small API.**
//!
//! By that rule the streaming, block, audio, and timing peripherals each get
//! their own shape rather than being bent onto `Bus`:
//!
//! | Traffic | Peripheral | The shape it wants |
//! |---|---|---|
//! | stream | UART | [`ByteStream`] — non-blocking byte counts, line errors |
//! | block | SD / SDIO | addressed 512-byte LBA sectors, not a byte payload |
//! | audio | I²S | continuous ping-pong DMA, no per-call transaction |
//! | timing | WS2812 / RMT | pulse-width symbols locked to a clock |
//!
//! Only the UART's `ByteStream` lives here today; the others are named so the
//! boundary is a decision on record, not a gap someone fills by reaching for
//! `Bus` again. Reference kernels sometimes do reach — Linux hangs a tty
//! discipline off a `uart_port`, Zephyr keeps `uart` entirely separate from its
//! `spi`/`i2c` drivers — and the ones that stay honest keep the stream apart
//! from the addressed bus, which is what this does.

/// Line conditions a receiver can latch between reads.
///
/// A stream keeps flowing through these — they are reported, not returned as an
/// error from [`ByteStream::read`], because dropping the whole read for one bad
/// byte would lose the good ones around it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamErrors {
    /// The receive buffer filled and bytes were lost.
    pub overrun: bool,
    /// A byte failed its parity check.
    pub parity: bool,
    /// A byte's stop bit was wrong — usually a baud-rate mismatch.
    pub framing: bool,
}

impl StreamErrors {
    /// Whether any error is set.
    pub fn any(&self) -> bool {
        self.overrun || self.parity || self.framing
    }
}

/// A byte-oriented, non-blocking stream: a UART, not an addressed bus.
///
/// [`write`](ByteStream::write) and [`read`](ByteStream::read) move as many
/// bytes as fit or are waiting **right now** and return the count — the caller
/// loops or buffers the rest. Neither blocks, so a stream never stalls a task
/// on a peer that is slow or silent. Line errors are polled separately via
/// [`errors`](ByteStream::errors) rather than folded into a read count.
pub trait ByteStream: Send + Sync {
    /// Write as many of `data` as fit in the transmit buffer now; return how
    /// many were taken. `0` means the buffer is full — try again later.
    fn write(&self, data: &[u8]) -> usize;

    /// Read what has arrived into `buf`, up to its length; return the count.
    /// `0` means nothing was waiting.
    fn read(&self, buf: &mut [u8]) -> usize;

    /// The line errors latched since this was last called, and clear them.
    fn errors(&self) -> StreamErrors;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_errors_is_the_default_and_reads_as_clear() {
        let e = StreamErrors::default();
        assert!(!e.any());
    }

    #[test]
    fn any_error_flags_true() {
        assert!(StreamErrors { parity: true, ..Default::default() }.any());
        assert!(StreamErrors { overrun: true, ..Default::default() }.any());
        assert!(StreamErrors { framing: true, ..Default::default() }.any());
    }
}
