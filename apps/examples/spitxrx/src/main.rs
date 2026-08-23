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
//!   Layer 1   esp32-spi   the controller's registers
//! ```
//!
//! # No wire needed
//!
//! MOSI and MISO are both routed to one free pad through the GPIO matrix, so
//! every byte clocked out arrives back on MISO — the same trick
//! `apps/tests/spidma` and the on-target self-tests use. Order matters: MOSI
//! first, then MISO, so the second route wins the pad's input. SCK and a
//! placeholder MISO come from the board's spare loopback pads; `init` wants
//! three distinct pins before MISO is folded onto the scratch pad.
//!
//! # Porting to a real device
//!
//! Drop the fold, route MOSI/MISO/SCK to the device's pins, add a Layer-3
//! driver, and hand it a `BusHandle::new(&spi_bus)` exactly as
//! `apps/examples/imu` hands one to its IMU driver. The Layer-1/Layer-2
//! bring-up below is unchanged.
//!
//! Next: `uartecho` — a UART is a stream, not a bus, and the template differs
//! accordingly. Or `imu` for the I²C version of this.

#![no_std]
#![no_main]

use api::bus::{Bus, Op};
use api::task;
use hal::pinmux::{PinConfig, PinMux, Signal};
use hal::types::Priority;
use soc_esp32::Esp32PinMux;

kernel::flint_app!(main, abi = 2);

/// SPI2's signal instance number, for the GPIO matrix. `board::loopback_spi`
/// opens SPI2, and the fold below routes its signals through the matrix.
const SPI2: u8 = 2;

fn main() {
    task::spawn("spitxrx", run, Priority::Normal(1), 4096);
}

fn run() {
    // The loopback needs a free pad for the data plus two spares. A board that
    // declares none cannot run this; say so rather than routing onto something.
    let Some(pads) = board::loopback_pads() else {
        api::log_error!(
            "[spitxrx] this board declares no free loopback GPIOs; \
             build for board-esp32-devkitc"
        );
        park();
    };

    api::log_info!(
        "[spitxrx] SPI2 looped MOSI->MISO on GPIO{}, SCK on GPIO{}",
        pads.scratch,
        pads.aux.0
    );

    // The board opens SPI2 on the loopback pads (three distinct pins, as `init`
    // wants). No `new(base)`, no `init`, no `static mut` here any more.
    let bus = match board::loopback_spi() {
        Ok(bus) => bus,
        Err(e) => {
            api::log_error!("[spitxrx] SPI bring-up failed: {:?}", e);
            park();
        }
    };

    // Fold MISO onto the MOSI pad through the matrix to make the on-chip
    // loopback: `init` refuses two signals on one pad, so this is done after
    // bring-up. MOSI first, then MISO, so the second route wins the pad's input.
    if fold_loopback(pads.scratch).is_none() {
        api::log_error!("[spitxrx] could not fold MISO onto the MOSI pad");
        park();
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

/// Fold SPI2's MOSI and MISO onto one pad through the GPIO matrix, making the
/// on-chip loopback. Routing is safe (ownership of the pad is the proof, #111),
/// so no `unsafe` and no driver bring-up here — the board did that.
fn fold_loopback(scratch: u8) -> Option<()> {
    let mux = Esp32PinMux::new();
    mux.route(Signal::SpiMosi(SPI2), scratch, PinConfig::PUSH_PULL).ok()?;
    mux.route(Signal::SpiMiso(SPI2), scratch, PinConfig::PUSH_PULL).ok()?;
    Some(())
}

fn park() -> ! {
    loop {
        task::sleep_ms(1000);
    }
}
