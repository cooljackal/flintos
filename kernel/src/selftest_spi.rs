// SPDX-License-Identifier: Apache-2.0

//! SPI-bus (Layer 2) loopback self-test, FIFO and DMA. Included by
//! [`crate::selftest`].
//!
//! Drives `SpiBus` over `Esp32Spi` with MOSI folded onto MISO on one pad, so
//! every byte clocked out returns on MISO. It exercises the transfer-list
//! `Bus` end to end and the driver's internal FIFO-vs-DMA decision: with DMA
//! enabled the driver runs every transfer over a descriptor chain (small and
//! large alike, matching esp-idf), and `set_dma(false)` forces the FIFO --
//! cap-sized, or chunked past the 64-byte cap.
//!
//! # The matrix
//!
//! The DMA path had a re-arm fault: the first transfer worked and any
//! consecutive one corrupted. So this runs a sequence of consecutive DMA
//! transfers of varying lengths and buffer contents, repeated several rounds,
//! requiring every one to round-trip byte-for-byte -- then a final pass with
//! DMA disabled to cover the chunked-FIFO opt-out.
//!
//! Needs three electrically-free pads: the scratch pad carries the folded
//! MOSI/MISO (`board::active::LOOPBACK_SCRATCH_GPIO`), plus a clock pad and a
//! placeholder MISO pad (`board::active::LOOPBACK_AUX_GPIOS`) that `init` wants
//! before MISO is folded onto the scratch pad. A board that declares either as
//! `None` skips this.

use super::Check;

/// Round-trip a sequence of DMA transfers over a looped `SpiBus`, byte-exact,
/// then the chunked-FIFO opt-out.
#[cfg(target_os = "none")]
pub(crate) fn spi_bus_loopback_round_trips(scratch: u8, sck: u8, miso_placeholder: u8) -> Check {
    use core::ptr::addr_of_mut;
    use esp32_spi::Esp32Spi;
    use hal::bus::{Bus, BusConfig, BusSpeed, Op, PhysicalBus, SpiMode};
    use hal::pinmux::{PinConfig, PinMux, Signal};
    use soc_esp32::{addr, Esp32PinMux};
    use spi_bus::SpiBus;

    /// SPI2's signal instance number.
    const SPI2: u8 = 2;
    /// Largest transfer in the matrix, in bytes. Word arrays keep the DMA
    /// buffers aligned; `static` puts them in DMA-reachable internal DRAM.
    const MAX: usize = 256;

    static mut SPI_DEV: Option<Esp32Spi> = None;
    static mut TXB: [u32; MAX / 4] = [0; MAX / 4];
    static mut RXB: [u32; MAX / 4] = [0; MAX / 4];

    let config = BusConfig::Spi {
        mosi: scratch,
        miso: miso_placeholder,
        sck,
        max_speed: BusSpeed::MHz(4),
        mode: SpiMode::Mode0,
    };

    let mut spi = unsafe { Esp32Spi::new(addr::SPI2_BASE) };
    spi.init(&config).map_err(|_| "SPI init failed -- the loopback pins would not route")?;

    let mux = Esp32PinMux::new();
    mux.route(Signal::SpiMosi(SPI2), scratch, PinConfig::PUSH_PULL)
        .map_err(|_| "could not route MOSI onto the scratch pad")?;
    mux.route(Signal::SpiMiso(SPI2), scratch, PinConfig::PUSH_PULL)
        .map_err(|_| "could not route MISO onto the scratch pad")?;

    let dev: &'static Esp32Spi = unsafe {
        let p = addr_of_mut!(SPI_DEV);
        p.write(Some(spi));
        (*p).as_ref().unwrap()
    };
    let bus = SpiBus::new(dev, config);

    let txb = unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(TXB) as *mut u8, MAX) };
    let rxb = unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(RXB) as *mut u8, MAX) };

    // A pattern that changes every transfer, so one returning a stale payload
    // (the re-arm bug's signature) cannot pass; prefill distinct from it too.
    let mut roundtrip = |n: usize, seed: u8| -> Result<(), &'static str> {
        for (i, b) in txb[..n].iter_mut().enumerate() {
            *b = seed.wrapping_add((i as u8).wrapping_mul(31));
        }
        rxb[..n].fill(seed ^ 0xFF);
        bus.transfer(&mut [Op::exchange(&txb[..n], &mut rxb[..n])])
            .map_err(|_| "an SPI-bus transfer failed")?;
        if rxb[..n] != txb[..n] {
            return Err("an SPI-bus transfer did not round-trip");
        }
        Ok(())
    };

    // Consecutive DMA transfers, small and large, over several rounds -- the
    // sequence the re-arm bug corrupted. Lengths span the old 64-byte cap so
    // the driver's small (<=64) and large paths are both exercised, all via DMA.
    let lens = [32usize, 128, 256, 16, 48, 100, 72, 64, 200];
    let mut seed = 1u8;
    for _round in 0..3 {
        for &n in lens.iter() {
            roundtrip(n, seed)?;
            seed = seed.wrapping_add(1);
        }
    }

    // The opt-out: DMA disabled, a past-cap transfer must still round-trip
    // through the FIFO in chunks. Done last, so no DMA transfer follows a FIFO
    // one on the same peripheral.
    dev.set_dma(false);
    roundtrip(200, seed)?;
    seed = seed.wrapping_add(1);

    // #91: the FIFO master path hung at low bus clocks — the SPI_CLOCK divider
    // overflowed its 6-bit counter instead of engaging the prescaler, leaving
    // h > n so SPI_CMD.usr never self-cleared. Re-clock to 1 MHz and drive the
    // driver's FIFO `transfer()` directly (the exact path that hung); it must
    // complete and round-trip byte-for-byte, not time out.
    dev.set_speed(BusSpeed::MHz(1)).map_err(|_| "could not reclock to 1 MHz")?;
    for (i, b) in txb[..32].iter_mut().enumerate() {
        *b = seed.wrapping_add((i as u8).wrapping_mul(31));
    }
    rxb[..32].fill(seed ^ 0xFF);
    dev.transfer(&txb[..32], &mut rxb[..32])
        .map_err(|_| "1 MHz FIFO transfer timed out -- the #91 divider bug")?;
    if rxb[..32] != txb[..32] {
        return Err("1 MHz FIFO transfer did not round-trip");
    }
    dev.set_speed(BusSpeed::MHz(4)).map_err(|_| "could not reclock back to 4 MHz")?;

    dev.set_dma(true);

    Ok(())
}

// Host stand-in: there is no SPI controller to drive.
#[cfg(not(target_os = "none"))]
pub(crate) fn spi_bus_loopback_round_trips(_scratch: u8, _sck: u8, _miso_placeholder: u8) -> Check {
    Ok(())
}
