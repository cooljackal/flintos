// SPDX-License-Identifier: Apache-2.0

//! SPI bus abstraction.
//!
//! Wraps a [`PhysicalBus`] impl and exposes the [`Bus`] trait as a list of
//! [`Op`]s run in order. A single op is capped at [`MAX_TRANSFER`] bytes — the
//! controller's data buffer — and the caller splits anything longer itself.

#![no_std]

use api::bus::{spin_rough_us, Bus, BusError, BusResult, BusSpeed, Op, PhysicalBus};

/// Largest single-op payload, bounded by the SPI data buffer (16 words).
const MAX_TRANSFER: usize = 64;

/// SPI bus abstraction.
pub struct SpiBus {
    phys: &'static dyn PhysicalBus,
    #[allow(dead_code)]
    config: api::bus::BusConfig,
}

impl SpiBus {
    /// Create a new SPI bus wrapping a physical driver.
    pub fn new(phys: &'static dyn PhysicalBus, config: api::bus::BusConfig) -> Self {
        Self { phys, config }
    }
}

impl Bus for SpiBus {
    fn transfer(&self, ops: &mut [Op]) -> BusResult<()> {
        for op in ops.iter_mut() {
            // The FIFO path is byte-oriented; wider words need the DMA path
            // (#80), not yet wired under the Bus.
            if op.word_bits != 8 {
                return Err(BusError::InvalidConfig);
            }
            match (op.tx, op.rx.as_deref_mut()) {
                (Some(tx), Some(rx)) => self.phys.raw_transfer(tx, rx)?,
                (Some(tx), None) => {
                    // A write still clocks a full duplex frame; the reply is
                    // discarded. The physical driver sends only min(tx, rx)
                    // bytes, so a matching-length scratch rx is what makes the
                    // whole of tx go out.
                    let mut scratch = [0u8; MAX_TRANSFER];
                    let n = tx.len().min(MAX_TRANSFER);
                    self.phys.raw_transfer(&tx[..n], &mut scratch[..n])?;
                }
                (None, Some(rx)) => {
                    // A read still has to clock: shift zeros out to shift the
                    // reply in, one buffer-full at a time.
                    let scratch = [0u8; MAX_TRANSFER];
                    let n = rx.len().min(MAX_TRANSFER);
                    self.phys.raw_transfer(&scratch[..n], &mut rx[..n])?;
                }
                (None, None) => {}
            }
            // Chip-select is the peripheral's own; there is no separate CS line
            // to hold here, so `op.cs` is advisory on this bus.
            if op.delay_us > 0 {
                spin_rough_us(op.delay_us);
            }
        }
        Ok(())
    }

    fn max_transfer(&self) -> usize {
        MAX_TRANSFER
    }

    fn set_speed(&self, speed: BusSpeed) -> BusResult<()> {
        self.phys.set_speed(speed)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use api::bus::{BusConfig, BusHandle, SpiMode};
    use core::sync::atomic::{AtomicU32, Ordering};
    use std::boxed::Box;
    use std::sync::Mutex;
    use std::vec::Vec;

    /// Echoes tx into rx and counts how many times it was re-clocked.
    struct MockSpi {
        last_speed_hz: AtomicU32,
    }

    impl PhysicalBus for MockSpi {
        fn init(&mut self, _: &BusConfig) -> BusResult<()> {
            Ok(())
        }
        fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            let len = tx.len().min(rx.len());
            rx[..len].copy_from_slice(&tx[..len]);
            Ok(())
        }
        fn set_speed(&self, speed: BusSpeed) -> BusResult<()> {
            self.last_speed_hz.store(speed.hz(), Ordering::Relaxed);
            Ok(())
        }
        fn set_enabled(&mut self, _: bool) {}
    }

    fn mock() -> &'static dyn PhysicalBus {
        std::boxed::Box::leak(std::boxed::Box::new(MockSpi { last_speed_hz: AtomicU32::new(0) }))
    }

    fn config() -> BusConfig {
        BusConfig::Spi {
            mosi: 23,
            miso: 19,
            sck: 18,
            max_speed: BusSpeed::MHz(1),
            mode: SpiMode::Mode0,
        }
    }

    #[test]
    fn exchange_echoes_tx_into_rx() {
        let phys = mock();
        let bus = SpiBus::new(phys, config());
        let mut rx = [0u8; 4];
        bus.transfer(&mut [Op::exchange(b"data", &mut rx)]).unwrap();
        assert_eq!(&rx, b"data");
    }

    #[test]
    fn a_read_op_clocks_zeros_and_fills_the_buffer() {
        let phys = mock();
        let bus = SpiBus::new(phys, config());
        let mut rx = [0xAAu8; 3];
        bus.transfer(&mut [Op::read(&mut rx)]).unwrap();
        // MockSpi echoes tx (zeros) into rx.
        assert_eq!(rx, [0u8; 3]);
    }

    #[test]
    fn a_non_byte_word_is_rejected() {
        let phys = mock();
        let bus = SpiBus::new(phys, config());
        let mut rx = [0u8; 2];
        assert_eq!(
            bus.transfer(&mut [Op::exchange(b"hi", &mut rx).with_word_bits(16)]),
            Err(BusError::InvalidConfig)
        );
    }

    #[test]
    fn set_speed_reaches_the_physical_driver() {
        let phys = mock();
        let bus = SpiBus::new(phys, config());
        assert!(bus.set_speed(BusSpeed::MHz(8)).is_ok());
    }

    /// Records the bytes the driver actually clocks out — which, exactly like
    /// the real hardware, is `min(tx, rx)` bytes. A plain echo mock takes the
    /// full `tx` slice as an argument and so cannot see a short-changed write.
    struct SendRecorder {
        sent: Mutex<Vec<u8>>,
    }

    impl PhysicalBus for SendRecorder {
        fn init(&mut self, _: &BusConfig) -> BusResult<()> {
            Ok(())
        }
        fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            let len = tx.len().min(rx.len());
            self.sent.lock().unwrap().extend_from_slice(&tx[..len]);
            rx[..len].copy_from_slice(&tx[..len]);
            Ok(())
        }
        fn set_enabled(&mut self, _: bool) {}
    }

    fn recording() -> (SpiBus, &'static SendRecorder) {
        let rec: &'static SendRecorder =
            Box::leak(Box::new(SendRecorder { sent: Mutex::new(Vec::new()) }));
        (SpiBus::new(rec, config()), rec)
    }

    #[test]
    fn a_write_only_op_sends_every_byte() {
        // Regression guard (#79): a write-only Op used to hand the physical
        // driver an empty rx, and the driver clocks only min(tx, rx) = 0 bytes,
        // so the write silently sent nothing. The wrapper must give a write a
        // matching-length scratch rx.
        let (bus, rec) = recording();
        bus.transfer(&mut [Op::write(b"cmd")]).unwrap();
        assert_eq!(&rec.sent.lock().unwrap()[..], b"cmd");
    }

    #[test]
    fn logical_drivers_still_reach_it_through_bushandle() {
        // The Layer-3 surface (write/read/transfer/write_read) is unchanged.
        let phys = mock();
        let bus: &'static SpiBus = std::boxed::Box::leak(std::boxed::Box::new(SpiBus::new(phys, config())));
        let handle = BusHandle::new(bus);
        let mut rx = [0u8; 4];
        assert!(handle.transfer(b"data", &mut rx).is_ok());
        assert_eq!(&rx, b"data");
        assert!(handle.write(b"cmd").is_ok());
        assert!(handle.read(&mut rx).is_ok());
        assert_eq!(handle.max_transfer(), MAX_TRANSFER);
    }
}
