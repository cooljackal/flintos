// SPDX-License-Identifier: Apache-2.0

//! UART bus abstraction.
//!
//! Wraps a [`PhysicalBus`] impl and exposes the [`Bus`] trait as a list of
//! [`Op`]s. A single op is capped at [`MAX_TRANSFER`] bytes; the caller splits
//! anything longer itself.

#![no_std]

use api::bus::{spin_rough_us, Bus, BusError, BusResult, BusSpeed, Op};
use api::PhysicalBus;

/// Largest single-op payload for the UART FIFO path.
const MAX_TRANSFER: usize = 256;

/// UART bus abstraction.
pub struct UartBus {
    phys: &'static dyn PhysicalBus,
}

impl UartBus {
    /// Create a new UART bus wrapping a physical driver.
    pub fn new(phys: &'static dyn PhysicalBus) -> Self {
        Self { phys }
    }
}

impl Bus for UartBus {
    fn transfer(&self, ops: &mut [Op]) -> BusResult<()> {
        for op in ops.iter_mut() {
            if op.word_bits != 8 {
                return Err(BusError::InvalidConfig);
            }
            match (op.tx, op.rx.as_deref_mut()) {
                (Some(tx), Some(rx)) => self.phys.raw_transfer(tx, rx)?,
                (Some(tx), None) => {
                    // The driver sends only min(tx, rx) bytes and drains the
                    // echo per byte; a matching-length scratch rx is what makes
                    // the whole write go out. The received bytes are discarded.
                    let mut scratch = [0u8; MAX_TRANSFER];
                    let n = tx.len().min(MAX_TRANSFER);
                    self.phys.raw_transfer(&tx[..n], &mut scratch[..n])?;
                }
                (None, Some(rx)) => {
                    // Clock out zeros to drive a same-length receive.
                    let scratch = [0u8; MAX_TRANSFER];
                    let n = rx.len().min(MAX_TRANSFER);
                    self.phys.raw_transfer(&scratch[..n], &mut rx[..n])?;
                }
                (None, None) => {}
            }
            // A UART has no chip-select; `op.cs` is ignored here.
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
    use api::bus::{BusConfig, BusHandle};
    use core::sync::atomic::{AtomicU32, Ordering};
    use std::boxed::Box;
    use std::sync::Mutex;
    use std::vec::Vec;

    struct MockUart {
        last_speed_hz: AtomicU32,
    }

    impl PhysicalBus for MockUart {
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
        std::boxed::Box::leak(std::boxed::Box::new(MockUart { last_speed_hz: AtomicU32::new(0) }))
    }

    #[test]
    fn write_and_read_ops_reach_the_driver() {
        let phys = mock();
        let bus = UartBus::new(phys);
        bus.transfer(&mut [Op::write(b"hello")]).unwrap();
        let mut buf = [0xAAu8; 4];
        bus.transfer(&mut [Op::read(&mut buf)]).unwrap();
        assert_eq!(&buf, &[0u8; 4]); // read clocks zeros
    }

    #[test]
    fn set_speed_reclocks_the_port() {
        let phys = mock();
        let bus = UartBus::new(phys);
        assert!(bus.set_speed(BusSpeed::KHz(9600 / 1000)).is_ok());
        assert!(bus.set_speed(BusSpeed::MHz(1)).is_ok());
    }

    /// Records the bytes the driver actually clocks out — `min(tx, rx)` bytes,
    /// like the real UART. A plain echo mock cannot see a short-changed write.
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

    fn recording() -> (UartBus, &'static SendRecorder) {
        let rec: &'static SendRecorder =
            Box::leak(Box::new(SendRecorder { sent: Mutex::new(Vec::new()) }));
        (UartBus::new(rec), rec)
    }

    #[test]
    fn a_write_only_op_sends_every_byte() {
        // Regression guard (#79): a write-only Op used to hand the physical
        // driver an empty rx, and the driver clocks only min(tx, rx) = 0 bytes,
        // so the write silently sent nothing. The wrapper must give a write a
        // matching-length scratch rx.
        let (bus, rec) = recording();
        bus.transfer(&mut [Op::write(b"hello")]).unwrap();
        assert_eq!(&rec.sent.lock().unwrap()[..], b"hello");
    }

    #[test]
    fn logical_surface_survives_through_bushandle() {
        let phys = mock();
        let bus: &'static UartBus = std::boxed::Box::leak(std::boxed::Box::new(UartBus::new(phys)));
        let handle = BusHandle::new(bus);
        let mut rx = [0u8; 4];
        assert!(handle.transfer(b"data", &mut rx).is_ok());
        assert_eq!(&rx, b"data");
        assert_eq!(handle.max_transfer(), MAX_TRANSFER);
    }
}
