// SPDX-License-Identifier: Apache-2.0

//! SPI-bus (Layer 2) loopback self-test. Included by [`crate::selftest`].
//!
//! Exercises the `spi-bus` wrapper on real silicon — until now it had no
//! consumer and was only unit-tested against a mock. An `Esp32Spi` is driven
//! through `SpiBus`, and MOSI is looped back onto MISO over one pad: point
//! SPI2's MOSI and MISO at the same GPIO through the matrix and every byte
//! clocked out arrives back on MISO. A byte-for-byte round trip proves the
//! wrapper, the FIFO transfer path, and the pin routing.
//!
//! FIFO path only — no DMA — so it stays at or under the 64-byte data buffer.
//!
//! Needs three electrically-free pads: the scratch pad carries the folded
//! MOSI/MISO (`board::active::LOOPBACK_SCRATCH_GPIO`), plus a clock pad and a
//! placeholder MISO pad (`board::active::SPI_LOOPBACK_AUX_GPIOS`) that `init`
//! wants before MISO is folded onto the scratch pad. A board that declares
//! either as `None` skips this.

use super::Check;

/// Loop a known pattern out MOSI and back in MISO through `SpiBus`, and require
/// it unchanged.
#[cfg(target_os = "none")]
pub(crate) fn spi_bus_loopback_round_trips(scratch: u8, sck: u8, miso_placeholder: u8) -> Check {
    use core::ptr::addr_of_mut;
    use esp32_spi::Esp32Spi;
    use hal::bus::{BusConfig, BusSpeed, Op, PhysicalBus, SpiMode};
    use hal::pinmux::{PinConfig, PinMux, Signal};
    use soc_esp32::{addr, Esp32PinMux};
    use spi_bus::SpiBus;

    /// SPI2's signal instance number.
    const SPI2: u8 = 2;
    const LEN: usize = 32; // under the 64-byte FIFO cap

    // The wrapper borrows the physical driver for `'static`; a self-test is a
    // one-shot at boot, so it lives here and is never taken again.
    static mut SPI_DEV: Option<Esp32Spi> = None;

    let config = BusConfig::Spi {
        mosi: scratch,
        miso: miso_placeholder,
        sck,
        max_speed: BusSpeed::MHz(4),
        mode: SpiMode::Mode0,
    };

    let mut spi = unsafe { Esp32Spi::new(addr::SPI2_BASE) };
    spi.init(&config).map_err(|_| "SPI init failed — the loopback pins would not route")?;

    // Fold MISO onto the MOSI pad. `init` rejects two signals on one pad on
    // purpose, so the loopback is made by routing directly afterwards. Order
    // matters: MOSI first, then MISO — the second route wins the pad's input.
    let mux = Esp32PinMux::new();
    mux.route(Signal::SpiMosi(SPI2), scratch, PinConfig::PUSH_PULL)
        .map_err(|_| "could not route MOSI onto the scratch pad")?;
    mux.route(Signal::SpiMiso(SPI2), scratch, PinConfig::PUSH_PULL)
        .map_err(|_| "could not route MISO onto the scratch pad")?;

    // Park the driver in the static and hand the wrapper a 'static borrow,
    // without ever forming a reference to the static itself.
    let phys: &'static dyn PhysicalBus = unsafe {
        let p = addr_of_mut!(SPI_DEV);
        p.write(Some(spi));
        (*p).as_ref().unwrap()
    };
    let bus = SpiBus::new(phys, config);

    // A recognisable ramp out; a receive buffer prefilled with something else so
    // a transfer that moves nothing cannot pass by coincidence.
    let mut tx = [0u8; LEN];
    for (i, b) in tx.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(31).wrapping_add(7);
    }
    let mut rx = [0xA5u8; LEN];

    use api::bus::Bus;
    bus.transfer(&mut [Op::exchange(&tx, &mut rx)])
        .map_err(|_| "the SPI-bus transfer failed")?;

    if rx != tx {
        return Err("the SPI-bus loopback data did not match what was sent");
    }
    Ok(())
}

// Host stand-in: there is no SPI controller to drive.
#[cfg(not(target_os = "none"))]
pub(crate) fn spi_bus_loopback_round_trips(_scratch: u8, _sck: u8, _miso_placeholder: u8) -> Check {
    Ok(())
}
