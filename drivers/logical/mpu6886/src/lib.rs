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
//! # Waiting
//!
//! The part needs pauses during its reset sequence. How to wait belongs to the
//! RTOS, not this driver — so [`Mpu6886::bring_up`] takes the wait as a
//! closure and owns only the *durations* (the datasheet's 10 ms minimums):
//!
//! ```ignore
//! dev.bring_up(api::task::sleep_ms)?;   // reset, wait, wake, wait, configure
//! ```
//!
//! The individual steps ([`reset`](Mpu6886::reset), [`wake`](Mpu6886::wake),
//! [`configure`](Mpu6886::configure)) stay public for a caller that must
//! interleave something else; each says how long to wait after it. The 10 ms
//! figures are the datasheet's minimum, not a recommendation for your board —
//! a board on a slowly-settling rail passes a longer-waiting closure.
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
pub struct Mpu6886<'a> {
    bus: BusHandle<'a>,
}

impl<'a> Mpu6886<'a> {
    /// Wrap a bus already addressed to the device.
    ///
    /// Takes anything that converts into a [`BusHandle`], so a caller passes a
    /// plain `&bus`: `Mpu6886::new(&bus)`.
    pub fn new(bus: impl Into<BusHandle<'a>>) -> Self {
        Self { bus: bus.into() }
    }

    fn write_reg(&self, reg: u8, val: u8) -> BusResult<()> {
        self.bus.write_reg(reg, val)
    }

    /// Read three big-endian 16-bit values starting at `reg`.
    ///
    /// One burst, not three reads: the part latches all six bytes when the
    /// first is read, so three separate reads can straddle a sample boundary
    /// and return two axes from one instant and one from the next.
    fn read_axes(&self, reg: u8) -> BusResult<Axes> {
        let mut b = [0u8; 6];
        self.bus.read_regs(reg, &mut b)?;
        Ok(Axes {
            x: i16::from_be_bytes([b[0], b[1]]),
            y: i16::from_be_bytes([b[2], b[3]]),
            z: i16::from_be_bytes([b[4], b[5]]),
        })
    }

    /// The identity register.
    pub fn who_am_i(&self) -> BusResult<u8> {
        self.bus.read_reg(REG_WHO_AM_I)
    }

    /// Whether an MPU6886 is answering.
    pub fn is_present(&self) -> BusResult<bool> {
        Ok(self.who_am_i()? == WHO_AM_I)
    }

    /// Clear the sleep bit, then soft-reset the part.
    ///
    /// **Wait at least 10 ms after this**, and again after [`Mpu6886::wake`].
    /// The part ignores the bus while resetting; carry on too early and it
    /// settles half-reset, answering `WHO_AM_I` correctly and returning zeros
    /// for motion.
    ///
    /// Sleep is cleared first because a reset issued while the part is asleep
    /// does not take.
    pub fn reset(&self) -> BusResult<()> {
        self.write_reg(REG_PWR_MGMT_1, 0x00)?;
        self.write_reg(REG_PWR_MGMT_1, PWR_DEVICE_RESET)
    }

    /// Select the PLL clock, which the datasheet prefers to the internal
    /// oscillator. Call after [`Mpu6886::reset`] and its delay.
    pub fn wake(&self) -> BusResult<()> {
        self.write_reg(REG_PWR_MGMT_1, PWR_CLK_PLL)
    }

    /// Configure for ±8 g and ±2000 °/s, polled, no FIFO.
    ///
    /// The full scales here are what [`Axes::to_milli_g`] and
    /// [`Axes::to_milli_dps`] assume. Changing one without the other makes
    /// every reading wrong by a factor, silently.
    pub fn configure(&self) -> BusResult<()> {
        self.write_reg(REG_ACCEL_CONFIG, ACCEL_FS_8G)?;
        self.write_reg(REG_GYRO_CONFIG, GYRO_FS_2000DPS)?;
        // DLPF 1: ~184 Hz bandwidth, and a 1 kHz sample rate divided by 6.
        self.write_reg(REG_CONFIG, 0x01)?;
        self.write_reg(REG_SMPLRT_DIV, 0x05)?;
        // Polled, so no interrupts and no FIFO.
        self.write_reg(REG_INT_ENABLE, 0x00)?;
        self.write_reg(REG_ACCEL_CONFIG2, 0x00)?;
        self.write_reg(REG_USER_CTRL, 0x00)?;
        self.write_reg(REG_FIFO_EN, 0x00)
    }

    /// Reset, wake, and configure the part, waiting between the steps.
    ///
    /// The full bring-up the caller used to open-code: [`reset`](Self::reset),
    /// wait, [`wake`](Self::wake), wait, [`configure`](Self::configure). The
    /// driver owns the sequence and the 10 ms datasheet minimums; `delay_ms`
    /// supplies *how* to wait, because that is the RTOS's business, not this
    /// driver's — pass `api::task::sleep_ms`.
    pub fn bring_up(&self, mut delay_ms: impl FnMut(u32)) -> BusResult<()> {
        self.reset()?;
        delay_ms(10);
        self.wake()?;
        delay_ms(10);
        self.configure()
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
        self.bus.read_regs(REG_TEMP_OUT_H, &mut b)?;
        let raw = i16::from_be_bytes([b[0], b[1]]) as i32;
        Ok(raw * 10_000 / 3268 + 25_000)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use api::testing::RegBus;
    use std::boxed::Box;
    use std::vec::Vec;

    fn imu(regs: &[(u8, u8)]) -> (Mpu6886<'static>, &'static RegBus) {
        let f: &'static RegBus = Box::leak(Box::new(RegBus::new(regs)));
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
    fn reset_clears_sleep_before_resetting() {
        // A reset issued while the part is asleep does not take, and the
        // symptom is a device that answers WHO_AM_I and returns zeros.
        let (m, f) = imu(&[(REG_WHO_AM_I, WHO_AM_I)]);
        m.reset().unwrap();
        m.wake().unwrap();

        let writes = f.writes();
        let pwr: Vec<u8> = writes.iter().filter(|(r, _)| *r == REG_PWR_MGMT_1).map(|(_, v)| *v).collect();
        assert_eq!(pwr, [0x00, PWR_DEVICE_RESET, PWR_CLK_PLL], "clear sleep, reset, then PLL");
    }

    #[test]
    fn bring_up_runs_reset_wake_configure_with_waits_between() {
        // The sequence the imu app used to drive by hand. bring_up owns the
        // order and the two 10 ms waits; the closure supplies how to wait.
        let (m, f) = imu(&[(REG_WHO_AM_I, WHO_AM_I)]);
        let mut waits = Vec::new();
        m.bring_up(|ms| waits.push(ms)).unwrap();

        // Two waits, both 10 ms, one after reset and one after wake.
        assert_eq!(waits, [10, 10]);
        // Power register saw clear-sleep, reset, then PLL — reset before the
        // first wait, wake before the second.
        let writes = f.writes();
        let pwr: Vec<u8> = writes.iter().filter(|(r, _)| *r == REG_PWR_MGMT_1).map(|(_, v)| *v).collect();
        assert_eq!(pwr, [0x00, PWR_DEVICE_RESET, PWR_CLK_PLL]);
        // And configure ran: the full scales are set.
        assert_eq!(f.get(REG_ACCEL_CONFIG), Some(ACCEL_FS_8G));
    }

    #[test]
    fn configure_selects_the_full_scales_the_conversions_assume() {
        // If these drift apart, every reading is silently wrong by a factor.
        let (m, f) = imu(&[]);
        m.configure().unwrap();
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
