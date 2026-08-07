// SPDX-License-Identifier: Apache-2.0

//! BME280 temperature / humidity / pressure sensor driver.
//!
//! Layer 3 logical driver — knows nothing about the bus or MCU register
//! layout. Communicates via a [`BusHandle`] provided at construction.
//!
//! # Transport
//!
//! The BME280 supports both I2C and SPI, but the *register addressing
//! convention differs between them*: on SPI, bit 7 of the address byte
//! selects read (1) or write (0); on I2C the register address is used
//! unmodified and the direction is carried by the bus's own start/address
//! phase (already handled below [`BusHandle`]). The register constants in
//! this file are written in their SPI *read* form (bit 7 set), matching the
//! BME280 datasheet's memory map. Because a bus-agnostic [`BusHandle`]
//! carries no notion of which physical transport underlies it, the caller
//! must say so explicitly via [`Transport`] at construction time — this
//! driver cannot infer it and will not guess.
//!
//! # Never fabricate a reading
//!
//! Every accessor here either returns a value derived from raw ADC counts
//! run through Bosch's published compensation formulas, or an explicit
//! [`BusError`]. There is no code path that manufactures a plausible-looking
//! number: reads before [`Bme280::init`] has captured calibration data fail
//! closed, and requesting humidity from a chip identified as a BMP280 (which
//! has no humidity sensor) fails closed rather than returning zero or noise.

#![no_std]
#![cfg_attr(not(test), forbid(unsafe_code))]
//
// The layer check reads the dependency graph, and raw MMIO needs no
// dependency -- a device driver could write 0x3FF44008 with `api` as its only
// dep and still pass. This is the line that makes "cannot reach hardware" true
// rather than aspirational.
//
// Scoped to non-test builds because the mock buses these crates test against
// use `unsafe` to extend a stack borrow to 'static (see `extend` below in
// bme280). That is test scaffolding and never ships; the shipping code in all
// three crates contains no `unsafe` at all.

use api::bus::{BusError, BusHandle, BusResult};

// ── BME280 register map (BME280 datasheet, register memory map) ───────────
//
// Every constant below is written in its *read* form: on SPI, bit 7 of the
// address byte is the read/write flag (1 = read), and Bosch's published
// register addresses already have bit 7 set for every documented register
// (0x88 and up). `Bme280::write_addr` clears bit 7 for SPI writes;
// `Bme280::read_addr` is a no-op on these constants. On I2C both forms are
// identical because I2C carries direction in the slave address, not the
// register address, so the constants are used unmodified either way.
const REG_ID: u8 = 0xD0;
const REG_RESET: u8 = 0xE0;
const REG_CTRL_HUM: u8 = 0xF2;
const REG_CTRL_MEAS: u8 = 0xF4;
const REG_CONFIG: u8 = 0xF5;
/// Burst-read start for pressure: press_msb/lsb/xlsb (0xF7-0xF9), followed
/// immediately by temp_msb/lsb/xlsb (0xFA-0xFC) and, on a true BME280,
/// hum_msb/lsb (0xFD-0xFE). Temperature is *not* at this address — it is
/// three bytes further in. A prior version of this driver read this
/// register and reported the raw pressure count as a temperature.
const REG_PRESS_MSB: u8 = 0xF7;
/// Start of the `dig_T1..dig_T3`/`dig_P1..dig_P9` calibration block
/// (0x88-0x9F), followed immediately by `dig_H1` at 0xA1 (0xA0 is reserved).
const REG_CALIB_00: u8 = 0x88;
/// Start of the `dig_H2..dig_H6` calibration block (0xE1-0xE7).
const REG_CALIB_26: u8 = 0xE1;

/// Genuine BME280: temperature, pressure, and humidity.
const CHIP_ID_BME280: u8 = 0x60;
/// BMP280: temperature and pressure only — no humidity sensor exists on
/// this part, so `read_humidity` must fail rather than fabricate a value.
const CHIP_ID_BMP280: u8 = 0x58;

/// Physical transport the [`BusHandle`] is wired to.
///
/// The BME280's register addressing convention depends on this: SPI reads
/// require bit 7 of the address byte set and writes require it clear, while
/// I2C uses the address unmodified for both. `BusHandle` itself carries no
/// transport information, so the driver cannot detect this automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    I2c,
    Spi,
}

/// Which physical part responded to the chip-ID read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// 0x60 — full temperature/pressure/humidity part.
    Bme280,
    /// 0x58 — temperature/pressure only, no humidity sensor.
    Bmp280,
}

/// Factory-programmed compensation trim values (BME280 datasheet, register
/// memory map 0x88-0xA1 and 0xE1-0xE7). Read once in [`Bme280::init`].
#[derive(Debug, Clone, Copy, Default)]
struct Calibration {
    dig_t1: u16,
    dig_t2: i16,
    dig_t3: i16,
    dig_p1: u16,
    dig_p2: i16,
    dig_p3: i16,
    dig_p4: i16,
    dig_p5: i16,
    dig_p6: i16,
    dig_p7: i16,
    dig_p8: i16,
    dig_p9: i16,
    dig_h1: u8,
    dig_h2: i16,
    dig_h3: u8,
    dig_h4: i16,
    dig_h5: i16,
    dig_h6: i8,
}

/// BME280 temperature/humidity/pressure sensor.
pub struct Bme280 {
    bus: BusHandle,
    transport: Transport,
    variant: Option<Variant>,
    calib: Option<Calibration>,
}

impl Bme280 {
    /// Create a new BME280 driver on the given bus handle.
    ///
    /// `transport` must match how this bus handle is actually wired
    /// (I2C or SPI) — see the module-level docs for why the driver cannot
    /// infer this itself.
    pub fn new(bus: BusHandle, transport: Transport) -> Self {
        Self { bus, transport, variant: None, calib: None }
    }

    /// Which part responded at [`Bme280::init`] time, if any.
    pub fn variant(&self) -> Option<Variant> {
        self.variant
    }

    /// Read the chip ID to verify presence (0x60 = BME280, 0x58 = BMP280).
    pub fn chip_id(&self) -> BusResult<u8> {
        let mut buf = [0u8; 1];
        self.read_regs(REG_ID, &mut buf)?;
        Ok(buf[0])
    }

    /// Initialise the sensor: verify the chip ID, load calibration trim
    /// values, and configure oversampling. Leaves the driver un-primed
    /// (`variant`/calibration unset) on any failure, so a partially
    /// initialised device can never be read from.
    pub fn init(&mut self) -> BusResult<()> {
        let id = self.chip_id()?;
        let variant = match id {
            CHIP_ID_BME280 => Variant::Bme280,
            CHIP_ID_BMP280 => Variant::Bmp280,
            _ => return Err(BusError::DeviceNotResponding),
        };

        self.write_reg(REG_RESET, 0xB6)?;
        let calib = self.read_calibration(variant)?;

        if variant == Variant::Bme280 {
            // Humidity oversampling x1. Per the datasheet, ctrl_hum only
            // takes effect after ctrl_meas is subsequently written.
            self.write_reg(REG_CTRL_HUM, 0x01)?;
        }
        // Temperature x1, pressure x1, normal mode.
        self.write_reg(REG_CTRL_MEAS, 0x27)?;
        // Standby 1000ms, filter off.
        self.write_reg(REG_CONFIG, 0xA0)?;

        self.variant = Some(variant);
        self.calib = Some(calib);
        Ok(())
    }

    /// Read temperature in degrees Celsius.
    pub fn read_temperature(&self) -> BusResult<f32> {
        let calib = self.calib.as_ref().ok_or(BusError::InvalidConfig)?;
        let (adc_t, _adc_p, _adc_h) = self.read_raw()?;
        let (_t_fine, temp_centi) = compensate_temperature(calib, adc_t);
        Ok(temp_centi as f32 / 100.0)
    }

    /// Read barometric pressure in hectopascals (hPa).
    pub fn read_pressure(&self) -> BusResult<f32> {
        let calib = self.calib.as_ref().ok_or(BusError::InvalidConfig)?;
        let (adc_t, adc_p, _adc_h) = self.read_raw()?;
        let (t_fine, _temp) = compensate_temperature(calib, adc_t);
        let raw_q24_8 = compensate_pressure(calib, adc_p, t_fine).ok_or(BusError::InvalidConfig)?;
        Ok(raw_q24_8 as f32 / 256.0 / 100.0)
    }

    /// Read relative humidity as a percentage (0.0-100.0).
    ///
    /// Fails with [`BusError::InvalidConfig`] on a BMP280 (identified via
    /// [`Bme280::variant`]), which has no humidity sensor — this driver
    /// will not report a fabricated humidity for hardware that cannot
    /// measure it.
    pub fn read_humidity(&self) -> BusResult<f32> {
        if self.variant != Some(Variant::Bme280) {
            return Err(BusError::InvalidConfig);
        }
        let calib = self.calib.as_ref().ok_or(BusError::InvalidConfig)?;
        let (adc_t, _adc_p, adc_h) = self.read_raw()?;
        let adc_h = adc_h.ok_or(BusError::InvalidConfig)?;
        let (t_fine, _temp) = compensate_temperature(calib, adc_t);
        let raw_q22_10 = compensate_humidity(calib, adc_h, t_fine);
        Ok(raw_q22_10 as f32 / 1024.0)
    }

    // ── Transport-aware register addressing ────────────────────────────

    fn read_addr(&self, reg: u8) -> u8 {
        match self.transport {
            // Documented addresses are already in read form (bit 7 set).
            Transport::Spi => reg | 0x80,
            Transport::I2c => reg,
        }
    }

    fn write_addr(&self, reg: u8) -> u8 {
        match self.transport {
            Transport::Spi => reg & 0x7F,
            Transport::I2c => reg,
        }
    }

    fn write_reg(&self, reg: u8, val: u8) -> BusResult<()> {
        self.bus.select()?;
        let result = self.bus.write(&[self.write_addr(reg), val]);
        self.bus.deselect()?;
        result
    }

    fn read_regs(&self, reg: u8, buf: &mut [u8]) -> BusResult<()> {
        self.bus.select()?;
        let result = self.bus.transfer(&[self.read_addr(reg)], buf);
        self.bus.deselect()?;
        result
    }

    /// Burst-read the raw ADC counts. Always reads pressure and temperature
    /// together (as the datasheet recommends) so that a subsequent
    /// pressure/humidity compensation uses a `t_fine` from the same sample
    /// rather than a stale one from a previous call.
    fn read_raw(&self) -> BusResult<(i32, i32, Option<i32>)> {
        let variant = self.variant.ok_or(BusError::InvalidConfig)?;
        match variant {
            Variant::Bme280 => {
                let mut buf = [0u8; 8];
                self.read_regs(REG_PRESS_MSB, &mut buf)?;
                let adc_p = raw20(buf[0], buf[1], buf[2]);
                let adc_t = raw20(buf[3], buf[4], buf[5]);
                let adc_h = ((buf[6] as i32) << 8) | (buf[7] as i32);
                Ok((adc_t, adc_p, Some(adc_h)))
            }
            Variant::Bmp280 => {
                let mut buf = [0u8; 6];
                self.read_regs(REG_PRESS_MSB, &mut buf)?;
                let adc_p = raw20(buf[0], buf[1], buf[2]);
                let adc_t = raw20(buf[3], buf[4], buf[5]);
                Ok((adc_t, adc_p, None))
            }
        }
    }

    fn read_calibration(&self, variant: Variant) -> BusResult<Calibration> {
        let mut low = [0u8; 26]; // 0x88..=0xA1 (dig_T*, dig_P*, reserved, dig_H1)
        self.read_regs(REG_CALIB_00, &mut low)?;

        let dig_t1 = u16::from_le_bytes([low[0], low[1]]);
        let dig_t2 = i16::from_le_bytes([low[2], low[3]]);
        let dig_t3 = i16::from_le_bytes([low[4], low[5]]);
        let dig_p1 = u16::from_le_bytes([low[6], low[7]]);
        let dig_p2 = i16::from_le_bytes([low[8], low[9]]);
        let dig_p3 = i16::from_le_bytes([low[10], low[11]]);
        let dig_p4 = i16::from_le_bytes([low[12], low[13]]);
        let dig_p5 = i16::from_le_bytes([low[14], low[15]]);
        let dig_p6 = i16::from_le_bytes([low[16], low[17]]);
        let dig_p7 = i16::from_le_bytes([low[18], low[19]]);
        let dig_p8 = i16::from_le_bytes([low[20], low[21]]);
        let dig_p9 = i16::from_le_bytes([low[22], low[23]]);
        // low[24] is 0xA0, reserved.
        let dig_h1 = low[25];

        let (dig_h2, dig_h3, dig_h4, dig_h5, dig_h6) = if variant == Variant::Bme280 {
            let mut high = [0u8; 7]; // 0xE1..=0xE7
            self.read_regs(REG_CALIB_26, &mut high)?;
            let dig_h2 = i16::from_le_bytes([high[0], high[1]]);
            let dig_h3 = high[2];
            // dig_H4/dig_H5 are packed 12-bit signed values sharing 0xE5:
            // dig_H4 = high[3]:high[4][3:0], dig_H5 = high[5]:high[4][7:4].
            let dig_h4 = ((high[3] as i8 as i16) << 4) | ((high[4] as i16) & 0x0F);
            let dig_h5 = ((high[5] as i8 as i16) << 4) | ((high[4] as i16) >> 4);
            let dig_h6 = high[6] as i8;
            (dig_h2, dig_h3, dig_h4, dig_h5, dig_h6)
        } else {
            // BMP280 has no humidity registers at these addresses; the
            // values are never consulted (`read_humidity` fails closed
            // for anything but Variant::Bme280), so leave them zeroed
            // rather than reading register contents that don't mean this.
            (0, 0, 0, 0, 0)
        };

        Ok(Calibration {
            dig_t1, dig_t2, dig_t3,
            dig_p1, dig_p2, dig_p3, dig_p4, dig_p5, dig_p6, dig_p7, dig_p8, dig_p9,
            dig_h1, dig_h2, dig_h3, dig_h4, dig_h5, dig_h6,
        })
    }
}

/// Assemble a burst-read 20-bit ADC count from msb/lsb/xlsb bytes.
fn raw20(msb: u8, lsb: u8, xlsb: u8) -> i32 {
    ((msb as i32) << 12) | ((lsb as i32) << 4) | ((xlsb as i32) >> 4)
}

// ── Bosch compensation formulas (BME280 datasheet §4.2.3, 32/64-bit fixed
// point integer variants — no floating point, matching the reference C
// driver Bosch ships in `bme280.c`). ────────────────────────────────────

/// Returns `(t_fine, temperature in 0.01 degC)`. `t_fine` is required by
/// both `compensate_pressure` and `compensate_humidity`.
fn compensate_temperature(calib: &Calibration, adc_t: i32) -> (i32, i32) {
    let dig_t1 = calib.dig_t1 as i32;
    let dig_t2 = calib.dig_t2 as i32;
    let dig_t3 = calib.dig_t3 as i32;

    let var1 = (((adc_t >> 3) - (dig_t1 << 1)) * dig_t2) >> 11;
    let var2 = (((((adc_t >> 4) - dig_t1) * ((adc_t >> 4) - dig_t1)) >> 12) * dig_t3) >> 14;
    let t_fine = var1 + var2;
    let temp = (t_fine * 5 + 128) >> 8;
    (t_fine, temp)
}

/// Returns pressure in Pa as Q24.8 fixed point (divide by 256 for Pa), or
/// `None` if `dig_P1` and the intermediate terms would divide by zero (the
/// documented guard in Bosch's reference implementation).
fn compensate_pressure(calib: &Calibration, adc_p: i32, t_fine: i32) -> Option<u32> {
    let dig_p1 = calib.dig_p1 as i64;
    let dig_p2 = calib.dig_p2 as i64;
    let dig_p3 = calib.dig_p3 as i64;
    let dig_p4 = calib.dig_p4 as i64;
    let dig_p5 = calib.dig_p5 as i64;
    let dig_p6 = calib.dig_p6 as i64;
    let dig_p7 = calib.dig_p7 as i64;
    let dig_p8 = calib.dig_p8 as i64;
    let dig_p9 = calib.dig_p9 as i64;

    let mut var1: i64 = (t_fine as i64) - 128000;
    let mut var2: i64 = var1 * var1 * dig_p6;
    var2 += (var1 * dig_p5) << 17;
    var2 += dig_p4 << 35;
    var1 = ((var1 * var1 * dig_p3) >> 8) + ((var1 * dig_p2) << 12);
    var1 = (((1i64 << 47) + var1) * dig_p1) >> 33;

    if var1 == 0 {
        // Would divide by zero below; the datasheet's own reference
        // implementation returns a sentinel rather than fabricating a
        // pressure reading, so we surface that as "no result" instead.
        return None;
    }

    let mut p: i64 = 1048576 - adc_p as i64;
    p = ((p << 31) - var2) * 3125 / var1;
    var1 = (dig_p9 * (p >> 13) * (p >> 13)) >> 25;
    var2 = (dig_p8 * p) >> 19;
    p = ((p + var1 + var2) >> 8) + (dig_p7 << 4);

    Some(p as u32)
}

/// Returns relative humidity in %RH as Q22.10 fixed point (divide by 1024
/// for %RH), clamped to the sensor's documented 0-100% range.
fn compensate_humidity(calib: &Calibration, adc_h: i32, t_fine: i32) -> u32 {
    let dig_h1 = calib.dig_h1 as i32;
    let dig_h2 = calib.dig_h2 as i32;
    let dig_h3 = calib.dig_h3 as i32;
    let dig_h4 = calib.dig_h4 as i32;
    let dig_h5 = calib.dig_h5 as i32;
    let dig_h6 = calib.dig_h6 as i32;

    let mut v_x1: i32 = t_fine - 76800;
    v_x1 = (((adc_h << 14) - (dig_h4 << 20) - (dig_h5 * v_x1) + 16384) >> 15)
        * (((((((v_x1 * dig_h6) >> 10) * (((v_x1 * dig_h3) >> 11) + 32768)) >> 10) + 2097152) * dig_h2 + 8192) >> 14);
    v_x1 -= ((((v_x1 >> 15) * (v_x1 >> 15)) >> 7) * dig_h1) >> 4;
    v_x1 = v_x1.clamp(0, 419_430_400); // 100% RH in Q22.10, per datasheet
    (v_x1 >> 12) as u32
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use api::bus::{Bus, BusResult, BusSpeed};
    use std::sync::Mutex;
    use std::vec::Vec;

    // ── Mock bus infrastructure ─────────────────────────────────────────
    //
    // `Bus` requires `Send + Sync`, so recorded writes use a `Mutex`
    // rather than a `RefCell` (which is not `Sync`).

    struct MockBmeBus {
        chip_id: u8,
        calib_low: [u8; 26],
        calib_high: [u8; 7],
        data: [u8; 8],
        writes: Mutex<Vec<u8>>,
    }

    impl Default for MockBmeBus {
        fn default() -> Self {
            Self {
                chip_id: CHIP_ID_BME280,
                calib_low: [0; 26],
                calib_high: [0; 7],
                data: [0; 8],
                writes: Mutex::new(Vec::new()),
            }
        }
    }

    impl Bus for MockBmeBus {
        fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            match tx.first().copied() {
                Some(REG_ID) => rx[0] = self.chip_id,
                Some(REG_CALIB_00) => rx.copy_from_slice(&self.calib_low),
                Some(REG_CALIB_26) => rx.copy_from_slice(&self.calib_high),
                Some(REG_PRESS_MSB) => {
                    let n = rx.len();
                    rx.copy_from_slice(&self.data[..n]);
                }
                _ => {}
            }
            Ok(())
        }
        fn write(&self, data: &[u8]) -> BusResult<()> {
            self.writes.lock().unwrap().extend_from_slice(data);
            Ok(())
        }
        fn read(&self, _buf: &mut [u8]) -> BusResult<()> { Ok(()) }
        fn set_speed(&self, _speed: BusSpeed) -> BusResult<()> { Err(BusError::InvalidConfig) }
        fn select(&self) -> BusResult<()> { Ok(()) }
        fn deselect(&self) -> BusResult<()> { Ok(()) }
    }

    fn le16(v: i32) -> [u8; 2] {
        [(v & 0xFF) as u8, ((v >> 8) & 0xFF) as u8]
    }

    /// A self-consistent calibration block: the same trim values used in
    /// `compensate_temperature_matches_bme280_worked_example` below, so the
    /// full-pipeline test can assert against the same hand-verified result.
    fn worked_example_calib_low() -> [u8; 26] {
        let mut buf = [0u8; 26];
        buf[0..2].copy_from_slice(&le16(27504)); // dig_T1
        buf[2..4].copy_from_slice(&le16(26435)); // dig_T2
        buf[4..6].copy_from_slice(&le16(-1000i32)); // dig_T3
        buf[6..8].copy_from_slice(&le16(36477)); // dig_P1
        buf[8..10].copy_from_slice(&le16(-10685i32)); // dig_P2
        buf[10..12].copy_from_slice(&le16(3024)); // dig_P3
        buf[12..14].copy_from_slice(&le16(2855)); // dig_P4
        buf[14..16].copy_from_slice(&le16(140)); // dig_P5
        buf[16..18].copy_from_slice(&le16(-7i32)); // dig_P6
        buf[18..20].copy_from_slice(&le16(15500)); // dig_P7
        buf[20..22].copy_from_slice(&le16(-14600i32)); // dig_P8
        buf[22..24].copy_from_slice(&le16(6000)); // dig_P9
        buf[25] = 75; // dig_H1
        buf
    }

    fn worked_example_calib_high() -> [u8; 7] {
        // dig_H2 = 384, dig_H3 = 0, dig_H4 = 280, dig_H5 = 0, dig_H6 = 30.
        // dig_H4 (280 = 0x118) and dig_H5 (0) share register 0xE5 per the
        // datasheet: high byte of each in its own register, low nibbles
        // packed together as (dig_H5_lo << 4) | dig_H4_lo.
        [
            0x80, 0x01, // dig_H2 = 384, LE
            0x00,       // dig_H3
            0x11,       // dig_H4 high byte (bits 11:4) = 0x11
            0x08,       // shared 0xE5: dig_H5_lo(0x0)<<4 | dig_H4_lo(0x8)
            0x00,       // dig_H5 high byte (bits 11:4) = 0x00
            30,         // dig_H6
        ]
    }

    // ── chip ID / variant detection ─────────────────────────────────────

    #[test]
    fn bme280_chip_id_ok() {
        let bus = MockBmeBus { chip_id: CHIP_ID_BME280, ..Default::default() };
        let handle = BusHandle::new(unsafe { extend(&bus) });
        let sensor = Bme280::new(handle, Transport::I2c);
        assert_eq!(sensor.chip_id(), Ok(CHIP_ID_BME280));
    }

    #[test]
    fn bme280_init_rejects_unknown_chip_id() {
        let bus = MockBmeBus { chip_id: 0xFF, ..Default::default() };
        let handle = BusHandle::new(unsafe { extend(&bus) });
        let mut sensor = Bme280::new(handle, Transport::I2c);
        assert_eq!(sensor.init(), Err(BusError::DeviceNotResponding));
        assert_eq!(sensor.variant(), None);
    }

    #[test]
    fn bme280_init_detects_bmp280_and_gates_humidity() {
        let bus = MockBmeBus {
            chip_id: CHIP_ID_BMP280,
            calib_low: worked_example_calib_low(),
            ..Default::default()
        };
        let handle = BusHandle::new(unsafe { extend(&bus) });
        let mut sensor = Bme280::new(handle, Transport::I2c);
        assert!(sensor.init().is_ok());
        assert_eq!(sensor.variant(), Some(Variant::Bmp280));
        // BMP280 has no humidity sensor: must fail closed, never fabricate.
        assert_eq!(sensor.read_humidity(), Err(BusError::InvalidConfig));
    }

    #[test]
    fn bme280_read_before_init_fails_closed() {
        let bus = MockBmeBus::default();
        let handle = BusHandle::new(unsafe { extend(&bus) });
        let sensor = Bme280::new(handle, Transport::I2c);
        assert_eq!(sensor.read_temperature(), Err(BusError::InvalidConfig));
    }

    // ── transport-aware addressing ──────────────────────────────────────

    #[test]
    fn spi_write_clears_read_bit_i2c_leaves_address_unmodified() {
        let bus = MockBmeBus {
            calib_low: worked_example_calib_low(),
            calib_high: worked_example_calib_high(),
            ..Default::default()
        };

        let handle_spi = BusHandle::new(unsafe { extend(&bus) });
        let mut spi_sensor = Bme280::new(handle_spi, Transport::Spi);
        assert!(spi_sensor.init().is_ok());
        // REG_RESET (0xE0) written over SPI must have bit 7 cleared: 0x60.
        {
            let writes = bus.writes.lock().unwrap();
            assert_eq!(&writes[0..2], &[0x60, 0xB6]);
        }
        bus.writes.lock().unwrap().clear();

        let handle_i2c = BusHandle::new(unsafe { extend(&bus) });
        let mut i2c_sensor = Bme280::new(handle_i2c, Transport::I2c);
        assert!(i2c_sensor.init().is_ok());
        // Over I2C the documented address (0xE0) is used unmodified.
        let writes = bus.writes.lock().unwrap();
        assert_eq!(&writes[0..2], &[0xE0, 0xB6]);
    }

    // Tests only ever run single-threaded on the host, and the mock bus
    // outlives every handle built from it within a test body — this local
    // helper just satisfies `BusHandle::new`'s `'static` bound without
    // reaching for a heavier static-storage pattern per test.
    unsafe fn extend<'a>(bus: &'a MockBmeBus) -> &'static MockBmeBus {
        core::mem::transmute::<&'a MockBmeBus, &'static MockBmeBus>(bus)
    }

    // ── compensation math ────────────────────────────────────────────────

    #[test]
    fn compensate_temperature_matches_bme280_worked_example() {
        // dig_T1=27504, dig_T2=26435, dig_T3=-1000, adc_T=519888 is the
        // widely-published BME280 compensation worked example. Hand-traced
        // through Bosch's integer formula: var1=128793, var2=-371,
        // t_fine=128422, T=(128422*5+128)>>8=2508 -> 25.08 degC.
        let calib = Calibration { dig_t1: 27504, dig_t2: 26435, dig_t3: -1000, ..Default::default() };
        let (t_fine, temp) = compensate_temperature(&calib, 519_888);
        assert_eq!(t_fine, 128_422);
        assert_eq!(temp, 2508);
    }

    #[test]
    fn compensate_pressure_matches_double_precision_reference() {
        let calib = Calibration {
            dig_p1: 36477, dig_p2: -10685, dig_p3: 3024, dig_p4: 2855, dig_p5: 140,
            dig_p6: -7, dig_p7: 15500, dig_p8: -14600, dig_p9: 6000,
            ..Default::default()
        };
        let t_fine = 128_422;
        let adc_p = 415_148;

        let fixed = compensate_pressure(&calib, adc_p, t_fine).expect("dig_P1 != 0");
        let reference = reference_compensate_pressure_f64(&calib, adc_p, t_fine);

        let fixed_pa = fixed as f64 / 256.0;
        assert!(
            (fixed_pa - reference).abs() < 1.0,
            "fixed-point {fixed_pa} Pa vs double-precision reference {reference} Pa"
        );
    }

    #[test]
    fn compensate_humidity_matches_double_precision_reference() {
        let calib = Calibration {
            dig_h1: 75, dig_h2: 384, dig_h3: 0, dig_h4: 280, dig_h5: 0, dig_h6: 30,
            ..Default::default()
        };
        let t_fine = 128_422;
        let adc_h = 32_741;

        let fixed = compensate_humidity(&calib, adc_h, t_fine);
        let reference = reference_compensate_humidity_f64(&calib, adc_h, t_fine);

        let fixed_pct = fixed as f64 / 1024.0;
        assert!(
            (fixed_pct - reference).abs() < 0.1,
            "fixed-point {fixed_pct}% vs double-precision reference {reference}%"
        );
    }

    #[test]
    fn compensate_humidity_clamps_to_valid_range() {
        // Real factory calibration trim is small (`dig_H2` etc. are never
        // anywhere near their signed-16-bit range on actual hardware) so
        // the fixed-point algorithm's overflow-prone terms stay in range,
        // exactly as Bosch's reference implementation assumes; push the
        // *raw ADC* reading to its extremes instead, which real firmware
        // does encounter, and confirm the result always clamps into
        // 0..=100% rather than wrapping negative or past 100.
        let calib = Calibration { dig_h1: 75, dig_h2: 384, dig_h3: 0, dig_h4: 280, dig_h5: 0, dig_h6: 30, ..Default::default() };

        let low = compensate_humidity(&calib, 0, 128_422);
        assert!((0.0..=100.0).contains(&(low as f64 / 1024.0)));

        let high = compensate_humidity(&calib, 65535, 128_422);
        assert!((0.0..=100.0).contains(&(high as f64 / 1024.0)));
    }

    /// Independent oracle: BME280 datasheet §4.2.3 double-precision
    /// pressure formula, transcribed directly (not derived from the
    /// fixed-point code above) so it can catch a shared transcription bug.
    fn reference_compensate_pressure_f64(calib: &Calibration, adc_p: i32, t_fine: i32) -> f64 {
        let (dig_p1, dig_p2, dig_p3, dig_p4, dig_p5, dig_p6, dig_p7, dig_p8, dig_p9) = (
            calib.dig_p1 as f64, calib.dig_p2 as f64, calib.dig_p3 as f64, calib.dig_p4 as f64,
            calib.dig_p5 as f64, calib.dig_p6 as f64, calib.dig_p7 as f64, calib.dig_p8 as f64,
            calib.dig_p9 as f64,
        );
        let mut var1 = (t_fine as f64 / 2.0) - 64000.0;
        let mut var2 = var1 * var1 * dig_p6 / 32768.0;
        var2 += var1 * dig_p5 * 2.0;
        var2 = (var2 / 4.0) + (dig_p4 * 65536.0);
        var1 = (dig_p3 * var1 * var1 / 524288.0 + dig_p2 * var1) / 524288.0;
        var1 = (1.0 + var1 / 32768.0) * dig_p1;
        if var1 == 0.0 {
            return 0.0;
        }
        let mut p = 1_048_576.0 - adc_p as f64;
        p = (p - (var2 / 4096.0)) * 6250.0 / var1;
        let var1_f = dig_p9 * p * p / 2_147_483_648.0;
        let var2_f = p * dig_p8 / 32768.0;
        p + (var1_f + var2_f + dig_p7) / 16.0
    }

    /// Independent oracle: BME280 datasheet §4.2.3 double-precision
    /// humidity formula.
    fn reference_compensate_humidity_f64(calib: &Calibration, adc_h: i32, t_fine: i32) -> f64 {
        let (dig_h1, dig_h2, dig_h3, dig_h4, dig_h5, dig_h6) = (
            calib.dig_h1 as f64, calib.dig_h2 as f64, calib.dig_h3 as f64,
            calib.dig_h4 as f64, calib.dig_h5 as f64, calib.dig_h6 as f64,
        );
        let mut var_h = t_fine as f64 - 76800.0;
        var_h = (adc_h as f64 - (dig_h4 * 64.0 + dig_h5 / 16384.0 * var_h))
            * (dig_h2 / 65536.0
                * (1.0 + dig_h6 / 67_108_864.0 * var_h * (1.0 + dig_h3 / 67_108_864.0 * var_h)));
        var_h *= 1.0 - dig_h1 * var_h / 524_288.0;
        var_h.clamp(0.0, 100.0)
    }

    // ── full pipeline via the mock bus ──────────────────────────────────

    #[test]
    fn bme280_full_pipeline_reports_worked_example_temperature() {
        let mut bus = MockBmeBus {
            calib_low: worked_example_calib_low(),
            calib_high: worked_example_calib_high(),
            ..Default::default()
        };
        // adc_T=519888 packed as press(3B, arbitrary)+temp(3B)+hum(2B).
        let adc_t = 519_888i32;
        bus.data = [
            0x00, 0x00, 0x00, // press_msb/lsb/xlsb (unused by this assertion)
            ((adc_t >> 12) & 0xFF) as u8,
            ((adc_t >> 4) & 0xFF) as u8,
            ((adc_t << 4) & 0xF0) as u8,
            0x7F, 0xE5, // hum_msb/lsb (arbitrary in-range value)
        ];

        let handle = BusHandle::new(unsafe { extend(&bus) });
        let mut sensor = Bme280::new(handle, Transport::I2c);
        assert!(sensor.init().is_ok());

        let temp = sensor.read_temperature().unwrap();
        assert!((temp - 25.08).abs() < 0.01, "got {temp}");

        // Pressure/humidity plumbing (addressing, burst read, compensation
        // wiring) must at least execute without error; exact values are
        // covered by the dedicated compensation tests above.
        assert!(sensor.read_pressure().is_ok());
        assert!(sensor.read_humidity().is_ok());
    }
}
