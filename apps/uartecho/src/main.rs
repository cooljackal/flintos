// SPDX-License-Identifier: Apache-2.0

//! UART as a `ByteStream`, looped TX→RX on-chip.
//!
//! **The UART template.** `apps/imu` shows the three-layer stack for I²C, an
//! *addressed bus*. A UART is not a bus — it is a byte stream, with no address,
//! no chip-select, and no rx-matches-tx (see `hal::stream`). So there is no
//! Layer-2 wrapper to insert: the driver exposes [`ByteStream`] directly, and
//! this app drives it.
//!
//! ```text
//!   stream API   ByteStream    non-blocking write/read counts, line errors
//!   Layer 1      esp32-uart    the controller's registers
//! ```
//!
//! # No wire needed
//!
//! UART2 (never UART0 — that is the console) is put in its internal loopback
//! mode (CONF0 bit 14), which routes TX→RX on-chip. The pins are still routed
//! for real, so bring-up is exercised, but the data does not depend on an
//! analog pad edge.
//!
//! Enabling the receiver latches one spurious byte into the RX FIFO; the bring-
//! up drains it, and each round drains any residue before echoing, so the read
//! stays aligned. See `kernel::selftest_uart` for the full story.
//!
//! # Porting to a real link
//!
//! Drop `set_loopback`, route TX/RX to the real pins, and read/write the stream
//! as below. `write` and `read` are non-blocking and return counts, so a real
//! peer that is slow or silent never stalls the task — loop over the remainder.

#![no_std]
#![no_main]

use core::ptr::addr_of;

use api::bus::{BusConfig, UartDataBits, UartParity, UartStopBits};
use api::stream::ByteStream;
use api::task;
use hal::types::Priority;
use soc_esp32::addr;

kernel::flint_app!(main, abi = 1);

use kernel::board::active as board;

/// UART2. UART0 is the console the harness reads; UART1's pads clash with the
/// SPI flash on many modules, so UART2 is the safe spare.
const UART_BASE: u32 = addr::UART2_BASE;

/// The driver outlives the setup call: the stream methods take `&self` and the
/// loop borrows it for the life of the program.
static mut UART: Option<esp32_uart::Esp32Uart> = None;

fn main() {
    task::spawn("uartecho", run, Priority::Normal(1), 4096);
}

fn run() {
    // The pins are only routed, not wired; still, a board that declares no free
    // pads has nothing safe to route onto, so it cannot run this.
    let (Some(tx_pin), Some((rx_pin, _))) =
        (board::LOOPBACK_SCRATCH_GPIO, board::LOOPBACK_AUX_GPIOS)
    else {
        api::log_error!(
            "[uartecho] this board declares no free loopback GPIOs; \
             build for board-esp32-devkitc"
        );
        park();
    };

    api::log_info!("[uartecho] UART2 internal loopback, TX pad GPIO{}", tx_pin);

    if unsafe { bring_up(tx_pin, rx_pin) }.is_none() {
        api::log_error!("[uartecho] UART bring-up failed");
        park();
    }

    let Some(uart) = (unsafe { (*addr_of!(UART)).as_ref() }) else {
        park();
    };

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
            api::log_info!("[uartecho] round {}: {:?} echoed OK", round, tx);
        } else {
            api::log_error!("[uartecho] round {}: sent {:?}, got {:?} ({} bytes)", round, tx, rx, got);
        }
        round = round.wrapping_add(1);
    }
}

/// Layer 1 bring-up: init the port, loop it internally, drain the enable-time
/// spurious byte.
///
/// # Safety
/// Claims UART2 and the two pads for the life of the program, and stores the
/// driver in a static the loop borrows.
unsafe fn bring_up(tx_pin: u8, rx_pin: u8) -> Option<()> {
    let mut uart = esp32_uart::Esp32Uart::new(UART_BASE);
    uart.init(&BusConfig::Uart {
        tx: tx_pin,
        rx: rx_pin,
        baud: 115_200,
        data_bits: UartDataBits::Bits8,
        parity: UartParity::None,
        stop_bits: UartStopBits::Stop1,
    })
    .ok()?;

    // Route TX→RX internally: a clean digital path, no pad edge to mis-frame.
    uart.set_loopback(true);
    UART = Some(uart);

    // Absorb the spurious byte the receiver latches when it comes up.
    let uart_ref: &'static esp32_uart::Esp32Uart = (*addr_of!(UART)).as_ref()?;
    let mut sink = [0u8; 8];
    while uart_ref.read(&mut sink) > 0 {}
    Some(())
}

fn park() -> ! {
    loop {
        task::sleep_ms(1000);
    }
}
