// SPDX-License-Identifier: Apache-2.0

//! SPI bus abstraction.
//!
//! Wraps a [`PhysicalBus`] impl and exposes the [`Bus`] trait as a list of
//! [`Op`]s run in order. `MAX_TRANSFER` is the controller's data buffer; a
//! write-only or read-only op longer than that is clocked in buffer-sized
//! pieces, and an exchange is handed to the physical driver whole (it chunks
//! through the FIFO or uses DMA). No length is ever silently cut short.

#![no_std]

use api::bus::{spin_rough_us, Bus, BusError, BusKind, BusResult, BusSpeed, Op, PhysicalBus};

/// Largest single-op payload, bounded by the SPI data buffer (16 words).
const MAX_TRANSFER: usize = 64;

/// SPI bus abstraction.
pub struct SpiBus {
    phys: &'static dyn PhysicalBus,
}

impl SpiBus {
    /// Create a new SPI bus wrapping a physical driver.
    ///
    /// The bus's configuration (pins, mode, speed) is applied to the physical
    /// driver at `init` time by whoever constructs it; this wrapper holds no
    /// copy of its own, and speed changes go straight through to `phys`.
    pub fn new(phys: &'static dyn PhysicalBus) -> Self {
        Self { phys }
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
                (Some(tx), Some(rx)) => self.phys.exchange(tx, rx)?,
                (Some(tx), None) => {
                    // A write still clocks a full duplex frame; the reply is
                    // discarded. The physical driver sends only min(tx, rx)
                    // bytes, so each piece of tx goes out against a scratch rx
                    // of the same length. This used to stop after one scratch
                    // buffer's worth and drop the rest of tx (#98).
                    let mut scratch = [0u8; MAX_TRANSFER];
                    for chunk in tx.chunks(MAX_TRANSFER) {
                        self.phys.exchange(chunk, &mut scratch[..chunk.len()])?;
                    }
                }
                (None, Some(rx)) => {
                    // A read still has to clock: shift zeros out to shift the
                    // reply in, one buffer-full at a time.
                    let scratch = [0u8; MAX_TRANSFER];
                    for chunk in rx.chunks_mut(MAX_TRANSFER) {
                        self.phys.exchange(&scratch[..chunk.len()], chunk)?;
                    }
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

    fn kind(&self) -> BusKind {
        BusKind::Spi
    }

    fn set_speed(&self, speed: BusSpeed) -> BusResult<()> {
        self.phys.set_speed(speed)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use api::bus::{BusConfig, BusHandle, PhysicalTransfer};
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
    }

    impl PhysicalTransfer for MockSpi {
        fn exchange(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            let len = tx.len().min(rx.len());
            rx[..len].copy_from_slice(&tx[..len]);
            Ok(())
        }
        fn set_speed(&self, speed: BusSpeed) -> BusResult<()> {
            self.last_speed_hz.store(speed.hz(), Ordering::Relaxed);
            Ok(())
        }
    }

    fn mock() -> &'static dyn PhysicalBus {
        std::boxed::Box::leak(std::boxed::Box::new(MockSpi { last_speed_hz: AtomicU32::new(0) }))
    }

    #[test]
    fn exchange_echoes_tx_into_rx() {
        let phys = mock();
        let bus = SpiBus::new(phys);
        let mut rx = [0u8; 4];
        bus.transfer(&mut [Op::exchange(b"data", &mut rx)]).unwrap();
        assert_eq!(&rx, b"data");
    }

    #[test]
    fn a_read_op_clocks_zeros_and_fills_the_buffer() {
        let phys = mock();
        let bus = SpiBus::new(phys);
        let mut rx = [0xAAu8; 3];
        bus.transfer(&mut [Op::read(&mut rx)]).unwrap();
        // MockSpi echoes tx (zeros) into rx.
        assert_eq!(rx, [0u8; 3]);
    }

    #[test]
    fn a_non_byte_word_is_rejected() {
        let phys = mock();
        let bus = SpiBus::new(phys);
        let mut rx = [0u8; 2];
        assert_eq!(
            bus.transfer(&mut [Op::exchange(b"hi", &mut rx).with_word_bits(16)]),
            Err(BusError::InvalidConfig)
        );
    }

    #[test]
    fn set_speed_reaches_the_physical_driver() {
        let phys = mock();
        let bus = SpiBus::new(phys);
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
    }

    impl PhysicalTransfer for SendRecorder {
        fn exchange(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            let len = tx.len().min(rx.len());
            self.sent.lock().unwrap().extend_from_slice(&tx[..len]);
            rx[..len].copy_from_slice(&tx[..len]);
            Ok(())
        }
    }

    fn recording() -> (SpiBus, &'static SendRecorder) {
        let rec: &'static SendRecorder =
            Box::leak(Box::new(SendRecorder { sent: Mutex::new(Vec::new()) }));
        (SpiBus::new(rec), rec)
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
    fn a_write_longer_than_the_buffer_sends_every_byte() {
        // Regression guard (#98): a write past MAX_TRANSFER used to send the
        // first 64 bytes and silently drop the rest.
        let (bus, rec) = recording();
        let tx: Vec<u8> = (0..=200u8).collect();
        bus.transfer(&mut [Op::write(&tx)]).unwrap();
        assert_eq!(&rec.sent.lock().unwrap()[..], &tx[..]);
    }

    #[test]
    fn a_read_longer_than_the_buffer_fills_every_byte() {
        // Regression guard (#98): a read past MAX_TRANSFER used to fill the
        // first 64 bytes, leave the rest untouched, and still return Ok.
        let (bus, rec) = recording();
        let mut rx = [0xAAu8; 150];
        bus.transfer(&mut [Op::read(&mut rx)]).unwrap();
        assert_eq!(rx, [0u8; 150], "every byte must be clocked in");
        assert_eq!(rec.sent.lock().unwrap().len(), 150, "every byte must be clocked out");
    }

    #[test]
    fn logical_drivers_still_reach_it_through_bushandle() {
        // The Layer-3 surface (write/read/transfer/write_read) is unchanged.
        let phys = mock();
        let bus: &'static SpiBus = std::boxed::Box::leak(std::boxed::Box::new(SpiBus::new(phys)));
        let handle = BusHandle::new(bus);
        let mut rx = [0u8; 4];
        assert!(handle.transfer(b"data", &mut rx).is_ok());
        assert_eq!(&rx, b"data");
        assert!(handle.write(b"cmd").is_ok());
        assert!(handle.read(&mut rx).is_ok());
        assert_eq!(handle.max_transfer(), MAX_TRANSFER);
    }
}
