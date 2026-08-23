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

use core::ptr::addr_of;

use api::bus::{Bus, BusConfig, BusSpeed, Op, PhysicalBus, SpiMode};
use api::task;
use hal::pinmux::{PinConfig, PinMux, Signal};
use hal::types::Priority;
use soc_esp32::{addr, Esp32PinMux};
use spi_bus::SpiBus;

kernel::flint_app!(main, abi = 1);

use kernel::board::active as board;

/// SPI2 (HSPI). SPI1 drives the boot flash; SPI2/SPI3 are the general ones.
const SPI_BASE: u32 = addr::SPI2_BASE;
/// SPI2's signal instance number, for the GPIO matrix.
const SPI2: u8 = 2;

/// Layers 1 and 2 outlive the setup calls: the `Bus` methods take `&self`, and
/// the loop below borrows the wrapper for the life of the program.
static mut PHYS: Option<esp32_spi::Esp32Spi> = None;
static mut BUS: Option<SpiBus> = None;

fn main() {
    task::spawn("spitxrx", run, Priority::Normal(1), 4096);
}

fn run() {
    // The loopback needs a free pad for the data plus two spares. A board that
    // declares none cannot run this; say so rather than routing onto something.
    let (Some(scratch), Some((sck, miso_placeholder))) =
        (board::LOOPBACK_SCRATCH_GPIO, board::LOOPBACK_AUX_GPIOS)
    else {
        api::log_error!(
            "[spitxrx] this board declares no free loopback GPIOs; \
             build for board-esp32-devkitc"
        );
        park();
    };

    api::log_info!("[spitxrx] SPI2 looped MOSI->MISO on GPIO{}, SCK on GPIO{}", scratch, sck);

    let config = BusConfig::Spi {
        mosi: scratch,
        miso: miso_placeholder,
        sck,
        max_speed: BusSpeed::MHz(4),
        mode: SpiMode::Mode0,
    };

    if unsafe { bring_up(&config, scratch) }.is_none() {
        api::log_error!("[spitxrx] SPI bring-up failed");
        park();
    }

    let Some(bus) = (unsafe { (*addr_of!(BUS)).as_ref() }) else {
        park();
    };

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

/// Layer 1 + Layer 2 bring-up, then fold MISO onto the MOSI pad.
///
/// # Safety
/// Claims SPI2 and the loopback pads for the life of the program, and stores
/// the driver and bus in statics the loop borrows.
unsafe fn bring_up(config: &BusConfig, scratch: u8) -> Option<()> {
    let mut phys = esp32_spi::Esp32Spi::new(SPI_BASE);
    PhysicalBus::init(&mut phys, config).ok()?;
    PHYS = Some(phys);

    // `init` rejects two signals on one pad on purpose; the loopback is made by
    // routing directly afterwards. MOSI first, then MISO.
    let mux = Esp32PinMux::new();
    mux.route(Signal::SpiMosi(SPI2), scratch, PinConfig::PUSH_PULL).ok()?;
    mux.route(Signal::SpiMiso(SPI2), scratch, PinConfig::PUSH_PULL).ok()?;

    let phys_ref: &'static dyn PhysicalBus = (*addr_of!(PHYS)).as_ref()?;
    BUS = Some(SpiBus::new(phys_ref));
    Some(())
}

fn park() -> ! {
    loop {
        task::sleep_ms(1000);
    }
}
