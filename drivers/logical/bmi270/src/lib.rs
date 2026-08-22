// SPDX-License-Identifier: Apache-2.0

//! Bosch BMI270 — 6-axis IMU.
//!
//! Layer 3: knows the part, not the chip driving it. It talks through a
//! [`BusHandle`] and has no idea whether an ESP32, an STM32 or a test mock is
//! underneath.
//!
//! # What this does
//!
//! Identifies the part and reads its status. It does **not** read motion yet:
//! a BMI270 will not produce acceleration or rate until an 8 KiB
//! configuration blob has been uploaded to it, which is a separate and much
//! larger job than talking to the thing.
//!
//! That is deliberate rather than unfinished. This driver exists to be the
//! first device in this tree ever assembled through all three layers — Layer 1
//! registers, Layer 2 transport, Layer 3 device — and reading a chip ID over a
//! real wire proves the whole path. Motion is a follow-up with its own issue.
//!
//! # Register facts
//!
//! From Bosch's own `BMI270_SensorAPI`, read rather than recalled:
//!
//! | | |
//! |---|---|
//! | `BMI2_CHIP_ID_ADDR` | `0x00` |
//! | `BMI270_CHIP_ID` | `0x24` |
//! | `BMI2_I2C_PRIM_ADDR` | `0x68` (SDO low) |
//! | `BMI2_I2C_SEC_ADDR` | `0x69` (SDO high) |
//!
//! # Telling it apart from an MPU6886
//!
//! M5Stack shipped the ATOM Matrix with an MPU6886 and later revisions with a
//! BMI270. **Both answer at 0x68**, so the address identifies nothing — only
//! the ID register does, and the two put it in different places. [`probe`]
//! reads both and says which is actually on the board, rather than picking a
//! side and reporting a confusing failure when wrong.

#![no_std]
#![cfg_attr(not(test), forbid(unsafe_code))]

use api::bus::{BusHandle, BusResult};

/// Primary address, SDO tied low. The ATOM Matrix wires it this way.
pub const ADDR_PRIMARY: u8 = 0x68;
/// Secondary address, SDO tied high.
pub const ADDR_SECONDARY: u8 = 0x69;

/// `BMI2_CHIP_ID_ADDR`.
pub const REG_CHIP_ID: u8 = 0x00;
/// `BMI270_CHIP_ID`.
pub const CHIP_ID: u8 = 0x24;

/// `INTERNAL_STATUS`, which reports whether the config blob loaded.
pub const REG_INTERNAL_STATUS: u8 = 0x21;

/// An MPU6886's `WHO_AM_I` register and its value.
///
/// Not this part, and deliberately here anyway: the two chips share an address
/// and a socket, so "which one is this" is a question this driver is the
/// natural place to answer. See [`probe`].
pub const MPU6886_REG_WHO_AM_I: u8 = 0x75;
/// `MPU6886 WHO_AM_I` value.
pub const MPU6886_WHO_AM_I: u8 = 0x19;

/// What answered at the address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    /// A BMI270, chip ID 0x24.
    Bmi270,
    /// An MPU6886 — the older ATOM Matrix IMU, same address, different part.
    Mpu6886,
    /// Something responded, but neither ID register held an expected value.
    /// Carries what `CHIP_ID` returned, because that is the useful clue.
    Unknown(u8),
}

/// A BMI270 on a bus.
pub struct Bmi270 {
    bus: BusHandle,
}

impl Bmi270 {
    /// Wrap a bus already addressed to the device.
    ///
    /// The address is the *bus's* business, not this driver's — a Layer-2 I2C
    /// bus is constructed for one device. This driver never names 0x68.
    pub const fn new(bus: BusHandle) -> Self {
        Self { bus }
    }

    /// The chip ID register.
    pub fn chip_id(&self) -> BusResult<u8> {
        self.bus.read_reg(REG_CHIP_ID)
    }

    /// Whether the part on the bus is a BMI270.
    pub fn is_present(&self) -> BusResult<bool> {
        Ok(self.chip_id()? == CHIP_ID)
    }

    /// Work out which of the two parts M5Stack ships is actually present.
    ///
    /// Reads the BMI270's ID register first, then the MPU6886's. A part that
    /// answers neither returns [`Identity::Unknown`] carrying what it did say,
    /// which is more use than a bare "not found".
    pub fn probe(&self) -> BusResult<Identity> {
        let id = self.chip_id()?;
        if id == CHIP_ID {
            return Ok(Identity::Bmi270);
        }
        if self.bus.read_reg(MPU6886_REG_WHO_AM_I)? == MPU6886_WHO_AM_I {
            return Ok(Identity::Mpu6886);
        }
        Ok(Identity::Unknown(id))
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use api::bus::{Bus, BusError, Op};
    use std::boxed::Box;
    use std::sync::Mutex;
    use std::vec::Vec;

    /// A bus that answers from a tiny register file and records what was asked.
    struct FakeDevice {
        regs: Vec<(u8, u8)>,
        asked: Mutex<Vec<u8>>,
    }

    impl Bus for FakeDevice {
        fn transfer(&self, ops: &mut [Op]) -> BusResult<()> {
            for op in ops.iter_mut() {
                if let (Some(tx), Some(rx)) = (op.tx, op.rx.as_deref_mut()) {
                    let reg = *tx.first().ok_or(BusError::InvalidConfig)?;
                    self.asked.lock().unwrap().push(reg);
                    let val = self.regs.iter().find(|(r, _)| *r == reg).map(|(_, v)| *v);
                    // A real device NAKs an address it does not implement;
                    // returning zeros would let a driver read a plausible value
                    // out of nothing.
                    rx[0] = val.ok_or(BusError::InvalidConfig)?;
                }
                // Writes (register configuration) are accepted silently.
            }
            Ok(())
        }
        fn max_transfer(&self) -> usize {
            64
        }
    }

    fn device(regs: &[(u8, u8)]) -> (Bmi270, &'static FakeDevice) {
        let d: &'static FakeDevice = Box::leak(Box::new(FakeDevice {
            regs: regs.to_vec(),
            asked: Mutex::new(Vec::new()),
        }));
        (Bmi270::new(BusHandle::new(d)), d)
    }

    #[test]
    fn a_bmi270_is_recognised_by_its_chip_id() {
        let (imu, _) = device(&[(REG_CHIP_ID, CHIP_ID)]);
        assert_eq!(imu.chip_id(), Ok(0x24));
        assert_eq!(imu.is_present(), Ok(true));
        assert_eq!(imu.probe(), Ok(Identity::Bmi270));
    }

    #[test]
    fn an_mpu6886_is_not_mistaken_for_a_bmi270() {
        // Both parts answer at 0x68 and M5Stack ships both, so an address
        // match proves nothing. This is the case that would otherwise present
        // as "the bus works but the chip ID is wrong".
        let (imu, _) = device(&[
            (REG_CHIP_ID, 0x00),
            (MPU6886_REG_WHO_AM_I, MPU6886_WHO_AM_I),
        ]);
        assert_eq!(imu.is_present(), Ok(false));
        assert_eq!(imu.probe(), Ok(Identity::Mpu6886));
    }

    #[test]
    fn an_unknown_part_reports_what_it_actually_said() {
        // "Not found" sends someone to check their wiring. The value it did
        // return says whether anything is talking at all.
        let (imu, _) = device(&[(REG_CHIP_ID, 0x42), (MPU6886_REG_WHO_AM_I, 0x00)]);
        assert_eq!(imu.probe(), Ok(Identity::Unknown(0x42)));
    }

    #[test]
    fn probing_checks_the_bmi270_before_the_mpu6886() {
        // Order matters only for cost, but a wrong order would read a register
        // the present part does not implement and NAK on a healthy board.
        let (imu, dev) = device(&[(REG_CHIP_ID, CHIP_ID)]);
        imu.probe().unwrap();
        assert_eq!(&dev.asked.lock().unwrap()[..], &[REG_CHIP_ID]);
    }

    #[test]
    fn a_bus_error_is_propagated_not_swallowed() {
        // A NAK means nothing is there. Reporting it as "chip id 0" would send
        // someone hunting for the wrong fault.
        let (imu, _) = device(&[]);
        assert!(imu.chip_id().is_err());
        assert!(imu.probe().is_err());
    }

    #[test]
    fn the_register_numbers_are_bosch_s() {
        // Quoted from BMI270_SensorAPI: bmi2_defs.h and bmi270.h.
        assert_eq!(REG_CHIP_ID, 0x00);
        assert_eq!(CHIP_ID, 0x24);
        assert_eq!(ADDR_PRIMARY, 0x68);
        assert_eq!(ADDR_SECONDARY, 0x69);
    }
}
