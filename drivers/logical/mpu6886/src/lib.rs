// SPDX-License-Identifier: Apache-2.0

//! InvenSense MPU6886 — 6-axis IMU.
//!
//! The part on the M5Stack Atom Matrix. Layer 3: it knows the device, not the
//! chip driving it, and talks through a [`BusHandle`].
//!
//! # Integers only
//!
//! Readings come back as raw `i16` counts, with conversions to milli-g and
//! milli-degrees-per-second done in integer arithmetic. No `f32` anywhere.
//!
//! That is a deliberate choice for a driver in an RTOS. A float in a device
//! driver is a float in whatever context calls it — including, eventually, an
//! interrupt handler — and on Xtensa that means the FPU registers join the set
//! that a context switch has to save. Milli-units carry more precision than
//! this part's noise floor, so nothing is lost.
//!
//! # Delays are the caller's
//!
//! [`Mpu6886::init`] takes a `delay_ms` callback rather than calling
//! `api::task::sleep_ms` itself. The reset sequence genuinely needs to wait —
//! the part is unresponsive for a few milliseconds after a soft reset — but a
//! driver that sleeps is a driver that only works from a task, and one that
//! links against the kernel cannot be unit-tested on a host at all.
//!
//! # Register facts
//!
//! From M5Stack's own `MPU6886.cpp`, the driver that runs this exact part on
//! this exact board — including the configuration values, which are choices
//! rather than datasheet constants.

#![no_std]
#![cfg_attr(not(test), forbid(unsafe_code))]

use api::bus::{BusHandle, BusResult};

/// I2C address with AD0 low, which is how the Atom Matrix wires it.
pub const ADDR: u8 = 0x68;

/// `WHO_AM_I`, and the value that identifies this part.
pub const REG_WHO_AM_I: u8 = 0x75;
/// The MPU6886's identity. A BMI270 in the same socket answers 0x24 from a
/// different register — see the `bmi270` driver.
pub const WHO_AM_I: u8 = 0x19;

const REG_SMPLRT_DIV: u8 = 0x19;
const REG_CONFIG: u8 = 0x1A;
const REG_GYRO_CONFIG: u8 = 0x1B;
const REG_ACCEL_CONFIG: u8 = 0x1C;
const REG_ACCEL_CONFIG2: u8 = 0x1D;
const REG_FIFO_EN: u8 = 0x23;
const REG_INT_ENABLE: u8 = 0x38;
const REG_ACCEL_XOUT_H: u8 = 0x3B;
const REG_TEMP_OUT_H: u8 = 0x41;
const REG_GYRO_XOUT_H: u8 = 0x43;
const REG_USER_CTRL: u8 = 0x6A;
const REG_PWR_MGMT_1: u8 = 0x6B;

/// `PWR_MGMT_1` bit 7: soft reset.
const PWR_DEVICE_RESET: u8 = 1 << 7;
/// `PWR_MGMT_1` clock select 1 — the PLL, which the datasheet recommends over
/// the internal oscillator for stability.
const PWR_CLK_PLL: u8 = 0x01;

/// `ACCEL_CONFIG` full-scale ±8 g, in bits 4:3.
const ACCEL_FS_8G: u8 = 0b10 << 3;
/// `GYRO_CONFIG` full-scale ±2000 °/s, in bits 4:3.
const GYRO_FS_2000DPS: u8 = 0b11 << 3;

/// A three-axis reading, in raw counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Axes {
    pub x: i16,
    pub y: i16,
    pub z: i16,
}

impl Axes {
    /// Acceleration in milli-g, at the ±8 g full scale this driver configures.
    ///
    /// 8 g over 32768 counts, as `raw * 8000 / 32768`, reduced to `* 125 / 512`
    /// so the intermediate fits an `i32` for every possible input.
    pub const fn to_milli_g(self) -> Self {
        Self {
            x: scale(self.x, 125, 512),
            y: scale(self.y, 125, 512),
            z: scale(self.z, 125, 512),
        }
    }

    /// Angular rate in milli-degrees per second, at ±2000 °/s.
    ///
    /// `raw * 2_000_000 / 32768`, reduced to `* 125 / 2`. That exceeds `i16`
    /// for large inputs, so it saturates rather than wrapping — a wrapped rate
    /// reads as a sudden spin in the opposite direction.
    pub const fn to_milli_dps(self) -> Self {
        Self {
            x: scale(self.x, 125, 2),
            y: scale(self.y, 125, 2),
            z: scale(self.z, 125, 2),
        }
    }
}

/// `raw * num / den` in `i32`, clamped back into `i16`.
const fn scale(raw: i16, num: i32, den: i32) -> i16 {
    let v = (raw as i32) * num / den;
    if v > i16::MAX as i32 {
        i16::MAX
    } else if v < i16::MIN as i32 {
        i16::MIN
    } else {
        v as i16
    }
}

/// An MPU6886 on a bus.
pub struct Mpu6886 {
    bus: BusHandle,
}

impl Mpu6886 {
    /// Wrap a bus already addressed to the device.
    pub const fn new(bus: BusHandle) -> Self {
        Self { bus }
    }

    fn read_reg(&self, reg: u8) -> BusResult<u8> {
        let mut buf = [0u8; 1];
        self.bus.transfer(&[reg], &mut buf)?;
        Ok(buf[0])
    }

    fn write_reg(&self, reg: u8, val: u8) -> BusResult<()> {
        self.bus.write(&[reg, val])
    }

    /// Read three big-endian 16-bit values starting at `reg`.
    ///
    /// One burst, not three reads: the part latches all six bytes when the
    /// first is read, so three separate reads can straddle a sample boundary
    /// and return two axes from one instant and one from the next.
    fn read_axes(&self, reg: u8) -> BusResult<Axes> {
        let mut b = [0u8; 6];
        self.bus.transfer(&[reg], &mut b)?;
        Ok(Axes {
            x: i16::from_be_bytes([b[0], b[1]]),
            y: i16::from_be_bytes([b[2], b[3]]),
            z: i16::from_be_bytes([b[4], b[5]]),
        })
    }

    /// The identity register.
    pub fn who_am_i(&self) -> BusResult<u8> {
        self.read_reg(REG_WHO_AM_I)
    }

    /// Whether an MPU6886 is answering.
    pub fn is_present(&self) -> BusResult<bool> {
        Ok(self.who_am_i()? == WHO_AM_I)
    }

    /// Reset, wake, and configure for ±8 g and ±2000 °/s.
    ///
    /// `delay_ms` must actually wait: the part ignores the bus for a few
    /// milliseconds after a soft reset, and skipping the pause leaves it in a
    /// half-reset state that answers `WHO_AM_I` and returns zeros for motion.
    pub fn init(&self, mut delay_ms: impl FnMut(u32)) -> BusResult<()> {
        // Clear sleep, then reset, then select the PLL. M5Stack's own driver
        // does exactly this dance, and the order matters -- a reset issued
        // while asleep does not take.
        self.write_reg(REG_PWR_MGMT_1, 0x00)?;
        delay_ms(10);
        self.write_reg(REG_PWR_MGMT_1, PWR_DEVICE_RESET)?;
        delay_ms(10);
        self.write_reg(REG_PWR_MGMT_1, PWR_CLK_PLL)?;
        delay_ms(10);

        self.write_reg(REG_ACCEL_CONFIG, ACCEL_FS_8G)?;
        self.write_reg(REG_GYRO_CONFIG, GYRO_FS_2000DPS)?;
        // DLPF 1: ~184 Hz bandwidth, and a 1 kHz sample rate divided by 6.
        self.write_reg(REG_CONFIG, 0x01)?;
        self.write_reg(REG_SMPLRT_DIV, 0x05)?;
        // Polled, so no interrupts and no FIFO.
        self.write_reg(REG_INT_ENABLE, 0x00)?;
        self.write_reg(REG_ACCEL_CONFIG2, 0x00)?;
        self.write_reg(REG_USER_CTRL, 0x00)?;
        self.write_reg(REG_FIFO_EN, 0x00)?;
        delay_ms(10);
        Ok(())
    }

    /// Acceleration, raw counts. ±8 g full scale.
    pub fn accel(&self) -> BusResult<Axes> {
        self.read_axes(REG_ACCEL_XOUT_H)
    }

    /// Angular rate, raw counts. ±2000 °/s full scale.
    pub fn gyro(&self) -> BusResult<Axes> {
        self.read_axes(REG_GYRO_XOUT_H)
    }

    /// Die temperature in milli-degrees Celsius.
    ///
    /// `raw / 326.8 + 25`, as integers. This is the *die*, not the room: it
    /// reads several degrees high because the part is warming itself.
    pub fn temperature_milli_c(&self) -> BusResult<i32> {
        let mut b = [0u8; 2];
        self.bus.transfer(&[REG_TEMP_OUT_H], &mut b)?;
        let raw = i16::from_be_bytes([b[0], b[1]]) as i32;
        Ok(raw * 10_000 / 3268 + 25_000)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use api::bus::{Bus, BusError, BusSpeed};
    use std::boxed::Box;
    use std::sync::Mutex;
    use std::vec::Vec;

    struct Fake {
        regs: Mutex<Vec<(u8, u8)>>,
        writes: Mutex<Vec<(u8, u8)>>,
    }

    impl Fake {
        fn get(&self, reg: u8) -> Option<u8> {
            self.regs.lock().unwrap().iter().find(|(r, _)| *r == reg).map(|(_, v)| *v)
        }
    }

    impl Bus for Fake {
        fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            let start = *tx.first().ok_or(BusError::InvalidConfig)?;
            for (i, out) in rx.iter_mut().enumerate() {
                *out = self.get(start + i as u8).ok_or(BusError::DeviceNotResponding)?;
            }
            Ok(())
        }
        fn write(&self, data: &[u8]) -> BusResult<()> {
            if data.len() == 2 {
                self.writes.lock().unwrap().push((data[0], data[1]));
                let mut regs = self.regs.lock().unwrap();
                match regs.iter_mut().find(|(r, _)| *r == data[0]) {
                    Some(e) => e.1 = data[1],
                    None => regs.push((data[0], data[1])),
                }
            }
            Ok(())
        }
        fn read(&self, _: &mut [u8]) -> BusResult<()> {
            Ok(())
        }
        fn set_speed(&self, _: BusSpeed) -> BusResult<()> {
            Ok(())
        }
        fn select(&self) -> BusResult<()> {
            Ok(())
        }
        fn deselect(&self) -> BusResult<()> {
            Ok(())
        }
    }

    fn imu(regs: &[(u8, u8)]) -> (Mpu6886, &'static Fake) {
        let f: &'static Fake = Box::leak(Box::new(Fake {
            regs: Mutex::new(regs.to_vec()),
            writes: Mutex::new(Vec::new()),
        }));
        (Mpu6886::new(BusHandle::new(f)), f)
    }

    #[test]
    fn the_part_is_identified_by_who_am_i() {
        let (m, _) = imu(&[(REG_WHO_AM_I, WHO_AM_I)]);
        assert_eq!(m.who_am_i(), Ok(0x19));
        assert_eq!(m.is_present(), Ok(true));
    }

    #[test]
    fn a_bmi270_in_the_same_socket_is_not_accepted() {
        // Both parts answer at 0x68. Only the ID register separates them, and
        // this board has shipped with each.
        let (m, _) = imu(&[(REG_WHO_AM_I, 0x24)]);
        assert_eq!(m.is_present(), Ok(false));
    }

    #[test]
    fn axes_are_big_endian() {
        // Getting this backwards gives readings that look like noise rather
        // than an error, and gravity points nowhere sensible.
        let (m, _) = imu(&[
            (REG_ACCEL_XOUT_H, 0x01), (REG_ACCEL_XOUT_H + 1, 0x02),
            (REG_ACCEL_XOUT_H + 2, 0xFF), (REG_ACCEL_XOUT_H + 3, 0xFE),
            (REG_ACCEL_XOUT_H + 4, 0x40), (REG_ACCEL_XOUT_H + 5, 0x00),
        ]);
        assert_eq!(m.accel(), Ok(Axes { x: 0x0102, y: -2, z: 0x4000 }));
    }

    #[test]
    fn a_full_scale_reading_converts_to_the_documented_range() {
        // Half of full scale at +/-8 g is 4 g. If the scale factor is wrong
        // the numbers still look plausible, which is why this is pinned.
        let a = Axes { x: 16384, y: -16384, z: 0 }.to_milli_g();
        assert_eq!(a.x, 4000);
        assert_eq!(a.y, -4000);
    }

    #[test]
    fn a_gyro_reading_saturates_instead_of_wrapping() {
        // 2000 dps is 2_000_000 milli-dps, far past i16. Wrapping would report
        // a fast spin as a fast spin the other way.
        let g = Axes { x: 32767, y: -32768, z: 0 }.to_milli_dps();
        assert_eq!(g.x, i16::MAX);
        assert_eq!(g.y, i16::MIN);
    }

    #[test]
    fn init_resets_before_selecting_a_clock() {
        // A reset issued while the part is asleep does not take, and the
        // symptom is a device that answers WHO_AM_I and returns zeros.
        let (m, f) = imu(&[(REG_WHO_AM_I, WHO_AM_I)]);
        let mut delays = 0;
        m.init(|_| delays += 1).unwrap();

        let writes = f.writes.lock().unwrap();
        let pwr: Vec<u8> = writes.iter().filter(|(r, _)| *r == REG_PWR_MGMT_1).map(|(_, v)| *v).collect();
        assert_eq!(pwr, [0x00, PWR_DEVICE_RESET, PWR_CLK_PLL], "wake, reset, then PLL");
        assert!(delays >= 3, "the reset sequence must actually wait");
    }

    #[test]
    fn init_selects_the_full_scales_the_conversions_assume() {
        // If these drift apart, every reading is silently wrong by a factor.
        let (m, f) = imu(&[]);
        m.init(|_| {}).unwrap();
        assert_eq!(f.get(REG_ACCEL_CONFIG), Some(ACCEL_FS_8G));
        assert_eq!(f.get(REG_GYRO_CONFIG), Some(GYRO_FS_2000DPS));
        assert_eq!(ACCEL_FS_8G >> 3, 0b10, "+/-8 g");
        assert_eq!(GYRO_FS_2000DPS >> 3, 0b11, "+/-2000 dps");
    }

    #[test]
    fn temperature_reads_room_ish_at_zero_counts() {
        let (m, _) = imu(&[(REG_TEMP_OUT_H, 0x00), (REG_TEMP_OUT_H + 1, 0x00)]);
        assert_eq!(m.temperature_milli_c(), Ok(25_000));
    }

    #[test]
    fn a_bus_error_is_propagated_not_read_as_zero() {
        let (m, _) = imu(&[]);
        assert!(m.who_am_i().is_err());
        assert!(m.accel().is_err());
    }
}
