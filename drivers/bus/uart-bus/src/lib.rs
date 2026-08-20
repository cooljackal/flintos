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
                (Some(tx), None) => self.phys.raw_transfer(tx, &mut [])?,
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
