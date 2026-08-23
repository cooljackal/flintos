// SPDX-License-Identifier: Apache-2.0

//! SPI through the Layer-2 `Bus`, looped MOSI→MISO on one pad.
//!
//! **The SPI porting template.** `apps/examples/imu` shows the three-layer
//! stack for I²C; this is its SPI counterpart — Layer 1 (`esp32-spi`) under
//! Layer 2 (`spi-bus`), driven through the transfer-list [`Bus`] API. There is
//! no Layer-3 device here: the point is the bus, so it talks to itself.
//!
//! ```text
//!   Layer 2   spi-bus     the transfer-list Bus (Op lists)
//!   Layer 1   esp32-spi   the controller's registers (owned by the board)
//! ```
//!
//! # No wire needed
//!
//! MOSI and MISO are both routed to one free pad through the GPIO matrix, so
//! every byte clocked out arrives back on MISO — the same trick
//! `apps/tests/spidma` and the on-target self-tests use. The board does both
//! halves: `board::loopback_spi()` opens SPI2 on three distinct pads (as `init`
//! wants), and `board::fold_spi_loopback()` then folds MISO onto the MOSI pad.
//! So this app names no SoC crate and no physical driver, and holds no
//! mutable statics.
//!
//! # Porting to a real device
//!
//! Drop the fold, route MOSI/MISO/SCK to the device's pins, add a Layer-3
//! driver, and hand it a `BusHandle::new(&spi_bus)` exactly as
//! `apps/examples/imu` hands one to its IMU driver. The Layer-1/Layer-2
//! bring-up behind `board::loopback_spi()` is unchanged.
//!
//! Next: `uartecho` — a UART is a stream, not a bus, and the template differs
//! accordingly. Or `imu` for the I²C version of this.

#![no_std]
#![no_main]

use api::bus::{Bus, Op};
use api::task::{self, Task};

kernel::flint_app!(main, abi = 2);

fn main() {
    if Task::new("spitxrx", run).spawn().is_none() {
        api::log_error!("could not start the spitxrx task");
    }
}

fn run() {
    // The loopback needs a free pad for the data plus two spares. A board that
    // declares none cannot run this; say so rather than routing onto something.
    let Some(pads) = board::loopback_pads() else {
        api::log_error!(
            "[spitxrx] this board declares no free loopback GPIOs; \
             build for board-esp32-devkitc"
        );
        task::exit();
    };

    api::log_info!(
        "[spitxrx] SPI2 looped MOSI->MISO on GPIO{}, SCK on GPIO{}",
        pads.scratch,
        pads.aux.0
    );

    // The board opens SPI2 on the loopback pads (three distinct pins, as `init`
    // wants). No `new(base)`, no `init`, no mutable statics here any more.
    let bus = match board::loopback_spi() {
        Ok(bus) => bus,
        Err(e) => {
            api::log_error!("[spitxrx] SPI bring-up failed: {:?}", e);
            task::exit();
        }
    };

    // Fold MISO onto the MOSI pad through the matrix to make the on-chip
    // loopback: `init` refuses two signals on one pad, so the board does this
    // after bring-up. Routing is safe (ownership of the pad is the proof, #111),
    // so the app just asks the board for it.
    if let Err(e) = board::fold_spi_loopback() {
        api::log_error!("[spitxrx] could not fold MISO onto the MOSI pad: {:?}", e);
        task::exit();
    }

    // Exchange a rolling pattern and check it comes back. A prefill distinct
    // from the pattern means a transfer that moves nothing cannot look like a
    // success.
    let mut round = 0u8;
    loop {
        task::sleep_ms(1000);

        let mut tx = [0u8; 8];
        for (i, b) in tx.iter_mut().enumerate() {
            *b = round.wrapping_add(i as u8).wrapping_mul(31).wrapping_add(7);
        }
        let mut rx = [0xA5u8; 8];

        match bus.transfer(&mut [Op::exchange(&tx, &mut rx)]) {
            Ok(()) if rx == tx => api::log_info!("[spitxrx] round {}: {:?} looped back OK", round, tx),
            Ok(()) => api::log_error!("[spitxrx] round {}: sent {:?}, got {:?}", round, tx, rx),
            Err(e) => api::log_error!("[spitxrx] round {}: transfer failed: {:?}", round, e),
        }
        round = round.wrapping_add(1);
    }
}
