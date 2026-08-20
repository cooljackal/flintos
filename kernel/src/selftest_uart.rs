// SPDX-License-Identifier: Apache-2.0

//! UART-bus (Layer 2) loopback self-test. Included by [`crate::selftest`].
//!
//! Exercises the `uart-bus` wrapper on real silicon — it had no consumer and
//! was only unit-tested against a mock. A spare UART (UART2 — never UART0, the
//! console the harness reads) is brought up on real pads, then its transmitter
//! is looped to its receiver internally, and a pattern is round-tripped through
//! `UartBus`. A byte-for-byte match proves the wrapper, the write-then-drain
//! transfer path (#77), and the peripheral bring-up.
//!
//! # Why internal loopback, not the pad-matrix trick
//!
//! UART has a dedicated internal loopback (CONF0 bit 14) that routes TX→RX
//! on-chip, so a self-test needs no folded pad and no wire. The pins are still
//! routed for real (so bring-up and routing are exercised), but the data path
//! does not depend on an analog pad edge — which for an async UART is worth
//! avoiding, since the receiver frames on start-bit edges rather than a shared
//! clock the way SPI does.
//!
//! # The spurious first byte
//!
//! Enabling the receiver latches one spurious byte into the RX FIFO before any
//! real data arrives — seen with both pad-matrix and internal loopback, so it
//! is an RX-enable artifact, not a pad one. The `#77` transfer path reads one
//! RX byte per TX byte, so that stray byte would shift the whole round trip by
//! one. The test settles and drains the RX FIFO after enabling loopback, before
//! the measured exchange.
//!
//! Gated on the same free pads as the other loopbacks so the suite stays
//! uniform; a board that declares none skips it.

use super::Check;

/// Round-trip a known pattern through `UartBus` over an internally-looped
/// UART2, and require it back byte-for-byte.
#[cfg(target_os = "none")]
pub(crate) fn uart_bus_loopback_round_trips(tx_pin: u8, rx_pin: u8) -> Check {
    use core::ptr::addr_of_mut;
    use esp32_uart::Esp32Uart;
    use hal::bus::{Bus, BusConfig, Op, PhysicalBus, UartDataBits, UartParity, UartStopBits};
    use uart_bus::UartBus;

    // One-shot at boot; the wrapper borrows the driver for `'static`.
    static mut UART_DEV: Option<Esp32Uart> = None;

    let uart = unsafe { Esp32Uart::new(soc_esp32::addr::UART2_BASE) };
    {
        // `init` takes `&mut`; bind briefly, configure, then move it read-only
        // into the static below.
        let mut uart = uart;
        uart.init(&BusConfig::Uart {
            tx: tx_pin,
            rx: rx_pin,
            baud: 115_200,
            data_bits: UartDataBits::Bits8,
            parity: UartParity::None,
            stop_bits: UartStopBits::Stop1,
        })
        .map_err(|_| "UART init failed — the loopback pins would not route")?;

        // Route TX→RX internally: a clean digital path, no pad edge to mis-frame.
        uart.set_loopback(true);

        // Enabling the receiver latches one spurious byte into the RX FIFO
        // before any real data. Let it settle and drain it, so the first byte
        // the exchange reads back is the first byte it actually sent.
        crate::selftest::spin_ticks(2);
        while uart.getc().is_some() {}

        let phys: &'static dyn PhysicalBus = unsafe {
            let p = addr_of_mut!(UART_DEV);
            p.write(Some(uart));
            (*p).as_ref().unwrap()
        };
        let bus = UartBus::new(phys);

        // A recognisable ramp out; a receive buffer prefilled with something
        // else so a transfer that moves nothing cannot pass by coincidence.
        let mut tx = [0u8; 8];
        for (i, b) in tx.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        let mut rx = [0xC3u8; 8];

        bus.transfer(&mut [Op::exchange(&tx, &mut rx)])
            .map_err(|_| "the UART-bus transfer failed")?;

        if rx != tx {
            return Err("the UART-bus loopback data did not match what was sent");
        }
    }
    Ok(())
}

// Host stand-in: there is no UART controller to drive.
#[cfg(not(target_os = "none"))]
pub(crate) fn uart_bus_loopback_round_trips(_tx_pin: u8, _rx_pin: u8) -> Check {
    Ok(())
}
