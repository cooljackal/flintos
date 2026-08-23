// SPDX-License-Identifier: Apache-2.0

//! UART as a `ByteStream`, looped TX→RX on-chip.
//!
//! **The UART template.** `apps/examples/imu` shows the three-layer stack for
//! I²C, an *addressed bus*. A UART is not a bus — it is a byte stream, with no
//! address, no chip-select, and no rx-matches-tx (see `hal::stream`). So
//! there is no Layer-2 wrapper to insert: the driver exposes [`ByteStream`]
//! directly, and this app drives it.
//!
//! ```text
//!   stream API   ByteStream    non-blocking write/read counts, line errors
//!   Layer 1      esp32-uart    the controller's registers (owned by the board)
//! ```
//!
//! # No wire needed
//!
//! UART2 (never UART0 — that is the console) is put in its internal loopback
//! mode, which routes TX→RX on-chip. The board does the bring-up
//! (`board::uart_loopback()`): it opens the port on the loopback pads, switches
//! on the loopback, and drains the spurious byte the receiver latches on
//! enable. The pins are still routed for real, so bring-up is exercised, but
//! the data does not depend on an analog pad edge. So this app names no SoC
//! crate and no physical driver, and holds no mutable statics.
//!
//! # Porting to a real link
//!
//! Give the board a real UART accessor (drop the loopback, route TX/RX to the
//! real pins) and read/write the stream as below. `write` and `read` are
//! non-blocking and return counts, so a real peer that is slow or silent never
//! stalls the task — loop over the remainder.
//!
//! Next: that is the last example. `apps/tests/` holds the PASS/FAIL
//! verification apps, and `apps/README.md` says how to start your own.

#![no_std]
#![no_main]

use api::task::{self, Task};

kernel::flint_app!(main, abi = 2);

fn main() {
    if Task::new("uartecho", run).spawn().is_none() {
        api::log_error!("could not start the uartecho task");
    }
}

fn run() {
    // The board opens UART2 in internal loopback on its free pads, drains the
    // enable-time spurious byte, and hands back a byte stream. A board that
    // declares no loopback pads returns an error rather than routing onto
    // something.
    let uart = match board::uart_loopback() {
        Ok(uart) => uart,
        Err(e) => {
            api::log_error!(
                "UART loopback bring-up failed ({:?}); \
                 build for board-esp32-devkitc",
                e
            );
            task::exit();
        }
    };

    api::log_info!("UART2 internal loopback");

    let mut round = 0u8;
    loop {
        task::sleep_ms(1000);

        // Discard any byte the receiver latched while the line was idle, so the
        // echo this round reads back is the byte it sent, not a leftover.
        let mut sink = [0u8; 8];
        while uart.read(&mut sink) > 0 {}

        let mut tx = [0u8; 8];
        for (i, b) in tx.iter_mut().enumerate() {
            *b = round.wrapping_add(i as u8).wrapping_mul(37).wrapping_add(11);
        }
        // write is non-blocking: for 8 bytes into a 128-byte FIFO it all fits,
        // but a real link would loop over what it did not take.
        let _ = uart.write(&tx);

        // Read the echo back, non-blocking, until the whole pattern arrives.
        let mut rx = [0u8; 8];
        let mut got = 0usize;
        let mut spins = 0u32;
        while got < rx.len() {
            got += uart.read(&mut rx[got..]);
            spins += 1;
            if spins > 100_000 {
                break;
            }
        }

        if got == rx.len() && rx == tx {
            api::log_info!("round {}: {:?} echoed OK", round, tx);
        } else {
            api::log_error!("round {}: sent {:?}, got {:?} ({} bytes)", round, tx, rx, got);
        }
        round = round.wrapping_add(1);
    }
}
