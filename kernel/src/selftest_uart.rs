// SPDX-License-Identifier: Apache-2.0

//! UART `ByteStream` loopback self-test. Included by [`crate::selftest`].
//!
//! Exercises the UART's [`ByteStream`] surface on real silicon. A UART is a
//! stream, not a `Bus` (see `hal::stream`), so it is driven through
//! `ByteStream::write`/`read` — non-blocking byte counts — rather than an
//! addressed transaction.
//!
//! A spare UART (UART2 — never UART0, the console the harness reads) is put in
//! its internal loopback mode (CONF0 bit 14), so every byte written comes back
//! to be read. A byte-for-byte match proves the driver's stream path and the
//! peripheral bring-up.
//!
//! # The spurious first byte
//!
//! Enabling the receiver latches one spurious byte into the RX FIFO before any
//! real data. The test drains the RX FIFO after enabling loopback, before the
//! measured round trip, so that stray byte does not shift the result.
//!
//! Gated on the same free pads as the other loopbacks so the suite stays
//! uniform; a board that declares none skips it.

use super::Check;

/// Round-trip a known pattern through the UART's `ByteStream` over an
/// internally-looped UART2, and require it back byte-for-byte.
#[cfg(target_os = "none")]
pub(crate) fn uart_bytestream_loopback_round_trips(tx_pin: u8, rx_pin: u8) -> Check {
    use esp32_uart::Esp32Uart;
    use hal::bus::BusConfig;
    use hal::stream::ByteStream;

    let mut uart = unsafe { Esp32Uart::new(soc_esp32::addr::UART2_BASE) };
    uart.init(&BusConfig::uart_8n1(tx_pin, rx_pin, 115_200))
        .map_err(|_| "UART init failed — the loopback pins would not route")?;

    // Route TX→RX internally: a clean digital path with no pad edge to mis-frame.
    uart.set_loopback(true);

    // Absorb the spurious byte the receiver latches when it comes up, so the
    // first byte read back is the first byte actually sent.
    crate::selftest::spin_ticks(2);
    let mut sink = [0u8; 8];
    while uart.read(&mut sink) > 0 {}

    // Send a recognisable ramp. `write` is non-blocking and returns the count
    // taken; the pattern is well under the 128-byte TX FIFO, so it all goes.
    let mut tx = [0u8; 8];
    for (i, b) in tx.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(37).wrapping_add(11);
    }
    if uart.write(&tx) != tx.len() {
        return Err("the UART did not accept the whole pattern");
    }

    // Read it back, non-blocking, until the whole pattern arrives or the wait
    // runs out — nothing external drives this line, so a miss means the loopback
    // never delivered.
    let mut rx = [0u8; 8];
    let mut got = 0usize;
    let mut spins = 0u32;
    while got < rx.len() {
        got += uart.read(&mut rx[got..]);
        if got == rx.len() {
            break;
        }
        spins += 1;
        if spins > 100_000 {
            return Err("the UART loopback did not return the whole pattern");
        }
    }

    if uart.errors().any() {
        return Err("the UART receiver latched a line error during the loopback");
    }
    if rx != tx {
        return Err("the UART loopback data did not match what was sent");
    }
    Ok(())
}

// Host stand-in: there is no UART controller to drive.
#[cfg(not(target_os = "none"))]
pub(crate) fn uart_bytestream_loopback_round_trips(_tx_pin: u8, _rx_pin: u8) -> Check {
    Ok(())
}
