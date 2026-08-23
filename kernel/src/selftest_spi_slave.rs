// SPDX-License-Identifier: Apache-2.0

//! On-chip SPI **master↔slave** loopback self-test. Included by
//! [`crate::selftest`].
//!
//! Configures SPI2 as a master and SPI3 as a slave and joins them through the
//! GPIO matrix with no external wire — a 4-wire loopback:
//!
//! ```text
//!   master SCK-out ──▶ slave SCK-in     (pad 0)
//!   master MOSI-out ─▶ slave MOSI-in    (pad 1)
//!   master MISO-in ◀─ slave MISO-out    (pad 2)
//!   master CS-out ──▶ slave CS-in      (pad 3)
//! ```
//!
//! The real CS edge is part of the contract: the slave commits MOSI data to its
//! buffer when CS deasserts. The master's setup/hold phases keep both edges
//! separated from the data clocks.
//!
//! The pinmux [`Signal`] variants fix a direction (`SpiSck` is an output), which
//! is right for the master but backwards for the slave, so the slave side is
//! wired with the low-level `gpio_matrix` primitives directly: `connect_input`
//! for its SCK/MOSI/CS inputs, `connect_output` for its MISO. On the classic
//! ESP32 a signal's input and output matrix indices coincide, so one index
//! serves both directions.
//!
//! Both directions are asserted byte-exact: the master must receive the slave's
//! preloaded reply, and the slave must receive the master's pattern.

use super::Check;

/// Master↔slave loopback over `[sck, mosi, miso, cs]`, both directions byte-exact.
#[cfg(target_os = "none")]
pub(crate) fn spi_master_slave_loopback_round_trips(pads: [u8; 4]) -> Check {
    use esp32_spi::{Esp32Spi, Esp32SpiSlave};
    use hal::bus::{BusConfig, BusSpeed, PhysicalBus, SpiMode};
    use hal::pinmux::PinPull;
    use hal::pinmux::{PinConfig, PinMux, Signal};
    use soc_esp32::{addr, gpio_matrix, io_mux, Esp32PinMux};

    let [sck, mosi, miso, cs] = pads;

    // ── Master: SPI2, ordinary init routes its output SCK/MOSI and input MISO.
    let config = BusConfig::spi_mode0(mosi, miso, sck, BusSpeed::MHz(4));
    let mut master = unsafe { Esp32Spi::new(addr::SPI2_BASE) };
    master
        .init(&config)
        .map_err(|_| "SPI2 master init failed -- the loopback pins would not route")?;
    // The FIFO path drives the clock here; DMA is not needed for <=64 bytes and
    // keeps the slave's completion timing simple.
    master.set_dma(false);

    // The master drives a real CS on the fourth pad: `init` disables all CS, so
    // re-enable CS0 and route it out. Each transaction then frames the slave with
    // a CS falling edge before the first clock -- without it the slave never
    // frames and reads back its own TX buffer.
    let mux = Esp32PinMux::new();
    mux.route(Signal::SpiCs(2), cs, PinConfig::PUSH_PULL)
        .map_err(|_| "could not route master CS")?;
    unsafe { master.enable_cs0() };

    // ── Slave: SPI3, registers only. The pins are wired below.
    let mut slave = unsafe { Esp32SpiSlave::new(addr::SPI3_BASE) };
    slave
        .init(SpiMode::Mode0)
        .map_err(|_| "SPI3 slave init failed")?;

    // Matrix indices for the slave's four signals. Input and output indices
    // coincide on this chip, so the same index works whichever direction the
    // slave uses the signal in.
    let sck_idx = gpio_matrix::signal_index(Signal::SpiSck(3)).ok_or("no SPI3 SCK index")?;
    let mosi_idx = gpio_matrix::signal_index(Signal::SpiMosi(3)).ok_or("no SPI3 MOSI index")?;
    let miso_idx = gpio_matrix::signal_index(Signal::SpiMiso(3)).ok_or("no SPI3 MISO index")?;
    let cs_idx = gpio_matrix::signal_index(Signal::SpiCs(3)).ok_or("no SPI3 CS index")?;

    // SAFETY: these pads are the board's declared free loopback trio; the master
    // init already put them under GPIO-matrix control. Re-enabling input on the
    // two the master drives lets the slave read them too — the same fold the
    // single-pad loopbacks rely on.
    unsafe {
        // Slave reads the master's SCK and MOSI: the master routed these as
        // outputs (input buffer off), so turn the pad input back on before
        // pointing the slave's inputs at them.
        io_mux::configure(sck, io_mux::gpio_function(sck), true, PinPull::None)
            .map_err(|_| "could not re-enable SCK pad input")?;
        io_mux::configure(mosi, io_mux::gpio_function(mosi), true, PinPull::None)
            .map_err(|_| "could not re-enable MOSI pad input")?;
        gpio_matrix::connect_input(sck_idx, sck, false).map_err(|_| "slave SCK-in route failed")?;
        gpio_matrix::connect_input(mosi_idx, mosi, false).map_err(|_| "slave MOSI-in route failed")?;

        // Slave drives MISO; the master already reads this pad (its MISO input).
        gpio_matrix::connect_output(miso, miso_idx, true, false)
            .map_err(|_| "slave MISO-out route failed")?;

        // Slave reads the master's CS on the fourth pad. The master drives it as
        // an output (input buffer off), so re-enable the pad input first, then
        // point the slave's CS input at it. CS is active-low, so no inversion.
        io_mux::configure(cs, io_mux::gpio_function(cs), true, PinPull::None)
            .map_err(|_| "could not re-enable CS pad input")?;
        gpio_matrix::connect_input(cs_idx, cs, false).map_err(|_| "slave CS-in route failed")?;
    }

    // One full-duplex exchange of `n` bytes, both directions checked.
    let exchange = |n: usize, seed: u8| -> Result<(), &'static str> {
        let mut mtx = [0u8; 64];
        let mut mrx = [0u8; 64];
        let mut stx = [0u8; 64];
        let mut srx = [0u8; 64];
        for i in 0..n {
            // Two distinct patterns so a direction returning the *other* side's
            // data (or a stale buffer) cannot pass.
            mtx[i] = seed.wrapping_add((i as u8).wrapping_mul(7));
            stx[i] = (seed ^ 0xA5).wrapping_add((i as u8).wrapping_mul(13));
        }
        // Prefill the receive buffers with neither pattern.
        mrx[..n].fill(0x5C);
        srx[..n].fill(0x3E);

        // Arm the slave first: it must be waiting before the master clocks.
        slave.arm(&stx[..n], n).map_err(|_| "slave arm failed")?;
        master
            .fifo_exchange(&mtx[..n], &mut mrx[..n])
            .map_err(|_| "master transfer failed")?;
        slave.complete(&mut srx[..n], n).map_err(|_| "slave completion timed out")?;

        if mrx[..n] != stx[..n] || srx[..n] != mtx[..n] {
            return Err("master/slave exchange mismatch");
        }
        Ok(())
    };

    // Lengths spanning single-word and multi-word buffer transfers, several
    // rounds with a changing seed so a stale-buffer re-arm bug cannot pass.
    let lens = [1usize, 2, 4, 7, 16, 31, 64];
    let mut seed = 0x11u8;
    for _round in 0..3 {
        for &n in lens.iter() {
            exchange(n, seed)?;
            seed = seed.wrapping_add(1);
        }
    }

    Ok(())
}

// Host stand-in: there is no SPI controller to drive.
#[cfg(not(target_os = "none"))]
pub(crate) fn spi_master_slave_loopback_round_trips(_pads: [u8; 4]) -> Check {
    Ok(())
}
