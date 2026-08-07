// SPDX-License-Identifier: Apache-2.0

//! I2C bus abstraction.
//!
//! Wraps a [`PhysicalBus`] impl with a fixed slave address and
//! exposes the [`Bus`] trait.  Messages are formatted as raw
//! I2C frames with the slave address in the first byte.

#![no_std]

use api::bus::{Bus, BusError, BusResult, BusSpeed, PhysicalBus};

/// I2C bus abstraction.
/// Largest payload one transfer can carry, bounded by the controller's FIFO.
const MAX_PAYLOAD: usize = 64;

pub struct I2cBus {
    phys: &'static dyn PhysicalBus,
    addr: u8,
}

impl I2cBus {
    /// Create a new I2C bus for `slave_addr`.
    pub fn new(phys: &'static dyn PhysicalBus, slave_addr: u8) -> Self {
        Self { phys, addr: slave_addr }
    }
}

impl Bus for I2cBus {
    // Every method passes the address UNSHIFTED as `tx[0]`; the physical
    // driver adds the R/W bit. See `hal::PhysicalBus::raw_transfer`. This crate
    // used to pre-shift in `write` and not in `transfer`, disagreeing with the
    // physical driver and with itself.
    fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        let mut buf = [0u8; MAX_PAYLOAD + 1];
        let len = tx.len().min(MAX_PAYLOAD);
        buf[0] = self.addr;
        buf[1..=len].copy_from_slice(&tx[..len]);
        // rx is passed through, so the caller's buffer is what gets filled.
        // The old version read into a throwaway array and dropped it.
        self.phys.raw_transfer(&buf[..=len], rx)
    }

    fn write(&self, data: &[u8]) -> BusResult<()> {
        let mut buf = [0u8; MAX_PAYLOAD + 1];
        let len = data.len().min(MAX_PAYLOAD);
        buf[0] = self.addr;
        buf[1..=len].copy_from_slice(&data[..len]);
        self.phys.raw_transfer(&buf[..=len], &mut [])
    }

    fn read(&self, buf: &mut [u8]) -> BusResult<()> {
        // Address only: no data bytes means a plain read, not a zero-length
        // write followed by one. The old version sent a zeroed tx, which
        // addressed the I2C general-call address rather than the device.
        self.phys.raw_transfer(&[self.addr], buf)
    }

    fn set_speed(&self, _speed: BusSpeed) -> BusResult<()> {
        Err(BusError::InvalidConfig)
    }

    fn select(&self) -> BusResult<()> {
        Ok(())
    }

    fn deselect(&self) -> BusResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use api::bus::BusConfig;
    use std::boxed::Box;
    use std::sync::Mutex;
    use std::vec::Vec;

    /// Records what reached the physical layer, and hands back canned bytes.
    ///
    /// The mock this replaced echoed `tx` into `rx`, and the tests only
    /// asserted `is_ok()`. That cannot tell a correct address from a doubled
    /// one, which is why the bus layer and the physical driver disagreed about
    /// shifting for as long as they did -- each was tested against a mock that
    /// shared its own author's assumption.
    struct Recorder {
        // Mutex, not RefCell: `PhysicalBus` is `Sync`.
        seen: Mutex<Vec<u8>>,
        canned: Vec<u8>,
    }

    impl PhysicalBus for Recorder {
        fn init(&mut self, _: &BusConfig) -> BusResult<()> {
            Ok(())
        }
        fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            *self.seen.lock().unwrap() = tx.to_vec();
            let n = rx.len().min(self.canned.len());
            rx[..n].copy_from_slice(&self.canned[..n]);
            Ok(())
        }
        fn set_enabled(&mut self, _: bool) {}
    }

    fn bus_with(canned: &[u8]) -> (I2cBus, &'static Recorder) {
        let rec: &'static Recorder = Box::leak(Box::new(Recorder {
            seen: Mutex::new(Vec::new()),
            canned: canned.to_vec(),
        }));
        (I2cBus::new(rec, 0x76), rec)
    }

    #[test]
    fn the_address_reaches_the_physical_layer_unshifted() {
        // The physical driver adds the R/W bit. Pre-shifting here sends 0xEC,
        // which that driver shifts again to 0xD8 -- an address no device
        // answers to, and a fault that looks like bad wiring.
        let (bus, rec) = bus_with(&[]);
        bus.write(&[0xF4, 0x27]).unwrap();
        assert_eq!(rec.seen.lock().unwrap()[0], 0x76, "address must not be pre-shifted");
    }

    #[test]
    fn all_three_methods_address_the_device_the_same_way() {
        // This crate used to pre-shift in `write` and not in `transfer`.
        for (name, run) in [("write", 0), ("read", 1), ("transfer", 2)] {
            let (bus, rec) = bus_with(&[0xAA; 4]);
            let mut rx = [0u8; 2];
            match run {
                0 => bus.write(&[0x01]).unwrap(),
                1 => bus.read(&mut rx).unwrap(),
                _ => bus.transfer(&[0x01], &mut rx).unwrap(),
            }
            assert_eq!(rec.seen.lock().unwrap()[0], 0x76, "{name} addressed differently");
        }
    }

    #[test]
    fn a_read_returns_the_bytes_to_the_caller() {
        // `transfer` used to read into a throwaway buffer and drop it, and
        // returned Ok -- so a sensor driver saw zeros and no error.
        let (bus, _) = bus_with(&[0xDE, 0xAD, 0xBE]);
        let mut buf = [0u8; 3];
        bus.read(&mut buf).unwrap();
        assert_eq!(buf, [0xDE, 0xAD, 0xBE]);
    }

    #[test]
    fn a_transfer_returns_the_bytes_to_the_caller() {
        let (bus, _) = bus_with(&[0x12, 0x34]);
        let mut buf = [0u8; 2];
        bus.transfer(&[0xF7], &mut buf).unwrap();
        assert_eq!(buf, [0x12, 0x34]);
    }

    #[test]
    fn a_plain_read_sends_the_address_and_nothing_else() {
        // A zeroed tx used to be sent instead, addressing the I2C general-call
        // address 0x00 rather than the device.
        let (bus, rec) = bus_with(&[0; 4]);
        let mut buf = [0u8; 4];
        bus.read(&mut buf).unwrap();
        assert_eq!(&rec.seen.lock().unwrap()[..], &[0x76], "read sends only the address");
    }

    #[test]
    fn a_write_carries_its_payload_after_the_address() {
        let (bus, rec) = bus_with(&[]);
        bus.write(&[0xF4, 0x27]).unwrap();
        assert_eq!(&rec.seen.lock().unwrap()[..], &[0x76, 0xF4, 0x27]);
    }
}
