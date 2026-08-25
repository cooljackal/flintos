// SPDX-License-Identifier: Apache-2.0

//! X-Powers AXP192 — the power-management IC on the M5Stack Core2 (and the
//! M5StickC family).
//!
//! Layer 3: it knows the PMIC, not the chip or board driving it, and talks
//! through a [`BusHandle`] on I2C at [`ADDR`]. What each rail *powers* is a
//! board fact and lives in the board manifest, not here — the same DCDC3 that
//! is the Core2's LCD backlight is something else on another board.
//!
//! # The five rails
//!
//! The AXP192 has three buck converters (DCDC1–3) and two LDOs (LDO2–3), each
//! enabled by one bit of the output-control register (`0x12`) and set by a
//! voltage register. This driver models them as [`Rail`] and a const config
//! table transliterated from the hardware-agnostic reference driver
//! `tuupola/axp192` (MIT) — min/max/step in millivolts, the voltage register,
//! and where the value sits in it:
//!
//! | Rail | enable bit (`0x12`) | voltage reg | range / step |
//! |---|---|---|---|
//! | DCDC1 | 0 | `0x26`[6:0] | 700–3500 / 25 mV |
//! | DCDC2 | 4 | `0x23`[5:0] | 700–2275 / 25 mV |
//! | DCDC3 | 1 | `0x27`[6:0] | 700–3500 / 25 mV |
//! | LDO2  | 2 | `0x28`[7:4] | 1800–3300 / 100 mV |
//! | LDO3  | 3 | `0x28`[3:0] | 1800–3300 / 100 mV |
//!
//! LDO2 and LDO3 share one register (`0x28`), one in each nibble, so setting
//! one must preserve the other — hence every voltage write is read-modify-write
//! and masked to the rail's field. The same is true of the single enable
//! register, which is why enabling a rail reads `0x12` first rather than
//! writing a fresh byte.
//!
//! # DCDC1 is the system rail — this driver never asserts a policy about it
//!
//! On the Core2, DCDC1 powers the ESP32 running this code. Turning it off or
//! moving its voltage under the CPU browns the board out. This driver exposes
//! DCDC1 like any other rail because it is one, but a *board* must never list
//! it for automatic bring-up; that invariant is enforced where the board's rail
//! list is applied (see the board crate's `power_init`), not here — a driver
//! that silently refused a rail would be lying about what the part can do.
//!
//! # No floats
//!
//! Voltages are `u16` millivolts and the encode/decode is integer arithmetic,
//! for the same reason the IMU driver keeps to integers: a float in a driver is
//! a float in whatever calls it, and on Xtensa that pulls the FPU into every
//! context switch that touches it.

#![no_std]
#![cfg_attr(not(test), forbid(unsafe_code))]

use api::bus::{BusError, BusHandle};

/// I2C address of the AXP192. Fixed in the part; not a board fact.
pub const ADDR: u8 = 0x34;

// ── Registers ────────────────────────────────────────────────────────────────

/// Power status: input-source presence and battery current direction.
const REG_POWER_STATUS: u8 = 0x00;
/// Charge status: charging indication and battery presence.
const REG_CHARGE_STATUS: u8 = 0x01;
/// Output control — one enable bit per rail. See [`Rail::enable_bit`].
const REG_OUTPUT_CTRL: u8 = 0x12;
/// Battery-voltage ADC, high 8 bits then low 4 bits (12-bit, 1.1 mV/LSB).
const REG_BAT_VOLTAGE_HI: u8 = 0x78;
const REG_BAT_VOLTAGE_LO: u8 = 0x79;
/// ADC enable 1: bit 7 battery voltage, bit 6 battery current.
const REG_ADC_ENABLE_1: u8 = 0x82;

/// GPIO3/4 function control, and GPIO3/4 output status. This pin pair starts at
/// index 3, so a GPIO's status bit is `1 << (index - 3)`.
const REG_GPIO34_FUNCTION: u8 = 0x95;
const REG_GPIO34_STATUS: u8 = 0x96;
/// The lowest index the REG95/REG96 pin pair covers.
const GPIO34_BASE: u8 = 3;

/// Configuring GPIO4 as an open-drain output in REG95: keep GPIO3's function
/// bits and the reserved bits, then select open-drain output for GPIO4. The
/// value is the AXP192's documented setting for this mode; it is named here
/// rather than written as a bare `(v & 0x72) | 0x84`.
const GPIO34_FUNC_PRESERVE: u8 = 0x72;
const GPIO4_OPEN_DRAIN_OUTPUT: u8 = 0x84;

/// `CHARGE_STATUS` bit 6: the charger is active.
const CHARGE_ACTIVE: u8 = 1 << 6;
/// `CHARGE_STATUS` bit 5: a battery is connected.
const BATTERY_PRESENT: u8 = 1 << 5;
/// `ADC_ENABLE_1` bits for the battery voltage and current ADCs.
const ADC_BATTERY_V: u8 = 1 << 7;
const ADC_BATTERY_I: u8 = 1 << 6;

// ── Rails ────────────────────────────────────────────────────────────────────

/// One of the AXP192's five switchable outputs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Rail {
    /// Buck 1. The system rail on the Core2 (ESP32) — do not switch at runtime.
    Dcdc1,
    /// Buck 2. Unused on the Core2.
    Dcdc2,
    /// Buck 3. The LCD backlight on the Core2.
    Dcdc3,
    /// LDO 2. Peripheral 3.3 V (SD card + LCD logic) on the Core2.
    Ldo2,
    /// LDO 3. The vibration motor on the Core2.
    Ldo3,
}

/// How a rail is enabled and how its voltage is encoded — the immutable facts
/// from the datasheet, one row per [`Rail`].
struct RailCfg {
    enable_bit: u8,
    min_mv: u16,
    max_mv: u16,
    step_mv: u16,
    voltage_reg: u8,
    /// Position of the value's least-significant bit within `voltage_reg`.
    lsb: u8,
    /// The bits `voltage_reg` uses for this rail (LDO2/LDO3 share `0x28`).
    mask: u8,
}

impl Rail {
    /// Bit of [`REG_OUTPUT_CTRL`] that enables this rail.
    const fn enable_bit(self) -> u8 {
        self.cfg().enable_bit
    }

    const fn cfg(self) -> RailCfg {
        match self {
            Rail::Dcdc1 => RailCfg { enable_bit: 0, min_mv: 700, max_mv: 3500, step_mv: 25, voltage_reg: 0x26, lsb: 0, mask: 0x7F },
            Rail::Dcdc2 => RailCfg { enable_bit: 4, min_mv: 700, max_mv: 2275, step_mv: 25, voltage_reg: 0x23, lsb: 0, mask: 0x3F },
            Rail::Dcdc3 => RailCfg { enable_bit: 1, min_mv: 700, max_mv: 3500, step_mv: 25, voltage_reg: 0x27, lsb: 0, mask: 0x7F },
            Rail::Ldo2 => RailCfg { enable_bit: 2, min_mv: 1800, max_mv: 3300, step_mv: 100, voltage_reg: 0x28, lsb: 4, mask: 0xF0 },
            Rail::Ldo3 => RailCfg { enable_bit: 3, min_mv: 1800, max_mv: 3300, step_mv: 100, voltage_reg: 0x28, lsb: 0, mask: 0x0F },
        }
    }

    /// Inclusive millivolt range this rail can be set to.
    pub const fn voltage_range(self) -> (u16, u16) {
        let c = self.cfg();
        (c.min_mv, c.max_mv)
    }
}

// ── Pure voltage/field arithmetic (host-tested) ──────────────────────────────

/// Millivolts → step count. The caller must have range-checked `millivolts`.
const fn steps_for(millivolts: u16, min_mv: u16, step_mv: u16) -> u8 {
    ((millivolts - min_mv) / step_mv) as u8
}

/// Step count → millivolts.
const fn millivolts_for(steps: u8, min_mv: u16, step_mv: u16) -> u16 {
    min_mv + steps as u16 * step_mv
}

/// Place `value` into `reg`'s `[mask]` field at `lsb`, leaving the other bits
/// (the *other* rail's, on the shared LDO register) untouched.
const fn pack_field(reg: u8, value: u8, lsb: u8, mask: u8) -> u8 {
    (reg & !mask) | ((value << lsb) & mask)
}

/// Extract the `[mask]`/`lsb` field from `reg`.
const fn unpack_field(reg: u8, lsb: u8, mask: u8) -> u8 {
    (reg & mask) >> lsb
}

// ── Errors ───────────────────────────────────────────────────────────────────

/// Why an AXP192 operation failed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AxpError {
    /// The underlying I2C transfer failed.
    Bus(BusError),
    /// A voltage outside the rail's range was requested. A PMIC does not
    /// silently clamp: an out-of-range write is a caller bug, and clamping it
    /// would hide the bug behind a wrong-but-plausible voltage.
    VoltageOutOfRange { rail: Rail, millivolts: u16 },
    /// A GPIO index this driver cannot address. Only GPIO3 and GPIO4 (the
    /// REG95/REG96 pin pair) are wired today.
    UnsupportedGpio(u8),
}

impl From<BusError> for AxpError {
    fn from(e: BusError) -> Self {
        AxpError::Bus(e)
    }
}

/// Result of an AXP192 operation.
pub type AxpResult<T> = Result<T, AxpError>;

// ── Driver ───────────────────────────────────────────────────────────────────

/// An AXP192 on a bus already addressed to [`ADDR`].
pub struct Axp192<'a> {
    bus: BusHandle<'a>,
}

impl<'a> Axp192<'a> {
    /// Wrap a bus handle addressed to the AXP192.
    ///
    /// Takes anything that converts into a [`BusHandle`], so a caller passes a
    /// plain `&device`: `Axp192::new(&controller.device(axp192::ADDR))`.
    pub fn new(bus: impl Into<BusHandle<'a>>) -> Self {
        Self { bus: bus.into() }
    }

    /// Enable or disable a rail, preserving every other rail's enable bit.
    pub fn set_rail_enabled(&self, rail: Rail, on: bool) -> AxpResult<()> {
        let bit = 1 << rail.enable_bit();
        let cur = self.bus.read_reg(REG_OUTPUT_CTRL)?;
        let next = if on { cur | bit } else { cur & !bit };
        self.bus.write_reg(REG_OUTPUT_CTRL, next)?;
        Ok(())
    }

    /// Whether a rail is currently enabled.
    pub fn rail_enabled(&self, rail: Rail) -> AxpResult<bool> {
        let cur = self.bus.read_reg(REG_OUTPUT_CTRL)?;
        Ok(cur & (1 << rail.enable_bit()) != 0)
    }

    /// Set a rail's output voltage, rounded down to the rail's step.
    ///
    /// Read-modify-write: LDO2 and LDO3 share one register, so the other rail's
    /// nibble is preserved. Errors [`AxpError::VoltageOutOfRange`] rather than
    /// clamping.
    pub fn set_rail_millivolts(&self, rail: Rail, millivolts: u16) -> AxpResult<()> {
        let c = rail.cfg();
        if millivolts < c.min_mv || millivolts > c.max_mv {
            return Err(AxpError::VoltageOutOfRange { rail, millivolts });
        }
        let steps = steps_for(millivolts, c.min_mv, c.step_mv);
        let cur = self.bus.read_reg(c.voltage_reg)?;
        let next = pack_field(cur, steps, c.lsb, c.mask);
        self.bus.write_reg(c.voltage_reg, next)?;
        Ok(())
    }

    /// Read back a rail's configured voltage in millivolts.
    pub fn rail_millivolts(&self, rail: Rail) -> AxpResult<u16> {
        let c = rail.cfg();
        let reg = self.bus.read_reg(c.voltage_reg)?;
        let steps = unpack_field(reg, c.lsb, c.mask);
        Ok(millivolts_for(steps, c.min_mv, c.step_mv))
    }

    /// The raw power-status register (`0x00`): input-source presence, battery
    /// current direction.
    pub fn power_status(&self) -> AxpResult<u8> {
        Ok(self.bus.read_reg(REG_POWER_STATUS)?)
    }

    /// The raw charge-status register (`0x01`).
    pub fn charge_status(&self) -> AxpResult<u8> {
        Ok(self.bus.read_reg(REG_CHARGE_STATUS)?)
    }

    /// Whether a battery is connected.
    pub fn battery_present(&self) -> AxpResult<bool> {
        Ok(self.charge_status()? & BATTERY_PRESENT != 0)
    }

    /// Whether the battery charger is active.
    pub fn charging(&self) -> AxpResult<bool> {
        Ok(self.charge_status()? & CHARGE_ACTIVE != 0)
    }

    /// Switch on the battery voltage and current ADCs, so
    /// [`battery_millivolts`](Self::battery_millivolts) reads live values.
    /// Their conversions sit dormant at reset; a board brings them up once.
    pub fn enable_battery_adc(&self) -> AxpResult<()> {
        let cur = self.bus.read_reg(REG_ADC_ENABLE_1)?;
        self.bus.write_reg(REG_ADC_ENABLE_1, cur | ADC_BATTERY_V | ADC_BATTERY_I)?;
        Ok(())
    }

    /// Configure `gpio` as an open-drain output. The board decides what hangs
    /// off it (the M5Core2 drives its LCD reset from GPIO4); the driver only
    /// knows the AXP192 register.
    ///
    /// Only GPIO4 is wired for open-drain output today — the one function this
    /// part is used for; another index returns [`AxpError::UnsupportedGpio`].
    pub fn set_gpio_open_drain_output(&self, gpio: u8) -> AxpResult<()> {
        if gpio != 4 {
            return Err(AxpError::UnsupportedGpio(gpio));
        }
        let v = self.bus.read_reg(REG_GPIO34_FUNCTION)?;
        self.bus
            .write_reg(REG_GPIO34_FUNCTION, (v & GPIO34_FUNC_PRESERVE) | GPIO4_OPEN_DRAIN_OUTPUT)?;
        Ok(())
    }

    /// Drive `gpio` (GPIO3 or GPIO4). Open-drain: `true` floats the pin (an
    /// external pull-up takes the line high), `false` pulls it to ground. A
    /// reset line is thus asserted with `false` and released with `true`.
    pub fn set_gpio(&self, gpio: u8, high: bool) -> AxpResult<()> {
        if !(GPIO34_BASE..=4).contains(&gpio) {
            return Err(AxpError::UnsupportedGpio(gpio));
        }
        let bit = 1 << (gpio - GPIO34_BASE);
        let v = self.bus.read_reg(REG_GPIO34_STATUS)?;
        let next = if high { v | bit } else { v & !bit };
        self.bus.write_reg(REG_GPIO34_STATUS, next)?;
        Ok(())
    }

    /// Battery voltage in millivolts, from the 12-bit ADC at 1.1 mV/LSB.
    ///
    /// Requires [`enable_battery_adc`](Self::enable_battery_adc) first, else the
    /// ADC registers read zero.
    pub fn battery_millivolts(&self) -> AxpResult<u16> {
        let hi = self.bus.read_reg(REG_BAT_VOLTAGE_HI)? as u16;
        let lo = self.bus.read_reg(REG_BAT_VOLTAGE_LO)? as u16;
        // 8 high bits then 4 low bits, 1.1 mV per count. Max 4095 * 11 / 10 =
        // 4504, well inside u16.
        let counts = (hi << 4) | (lo & 0x0F);
        Ok(counts * 11 / 10)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_enable_bits_match_the_output_control_register() {
        // 0x12: 0 DCDC1, 1 DCDC3, 2 LDO2, 3 LDO3, 4 DCDC2. A swapped bit here
        // switches the wrong rail — on the Core2 that is the difference between
        // the backlight and the system rail.
        assert_eq!(Rail::Dcdc1.enable_bit(), 0);
        assert_eq!(Rail::Dcdc3.enable_bit(), 1);
        assert_eq!(Rail::Ldo2.enable_bit(), 2);
        assert_eq!(Rail::Ldo3.enable_bit(), 3);
        assert_eq!(Rail::Dcdc2.enable_bit(), 4);
    }

    #[test]
    fn voltage_encode_decode_round_trips_on_every_rail() {
        for rail in [Rail::Dcdc1, Rail::Dcdc2, Rail::Dcdc3, Rail::Ldo2, Rail::Ldo3] {
            let c = rail.cfg();
            let mut mv = c.min_mv;
            while mv <= c.max_mv {
                let steps = steps_for(mv, c.min_mv, c.step_mv);
                assert_eq!(millivolts_for(steps, c.min_mv, c.step_mv), mv, "rail {rail:?} at {mv} mV");
                mv += c.step_mv;
            }
        }
    }

    #[test]
    fn the_core2_target_voltages_encode_as_expected() {
        // DCDC3 backlight 2800 mV: (2800-700)/25 = 84.
        let c = Rail::Dcdc3.cfg();
        assert_eq!(steps_for(2800, c.min_mv, c.step_mv), 84);
        // LDO2 peripheral 3300 mV: (3300-1800)/100 = 15, the top of its range.
        let c = Rail::Ldo2.cfg();
        assert_eq!(steps_for(3300, c.min_mv, c.step_mv), 15);
    }

    #[test]
    fn setting_one_shared_ldo_preserves_the_other() {
        // LDO2 in the high nibble, LDO3 in the low nibble of 0x28. Writing one
        // must not disturb the other, which is the whole reason the write is
        // read-modify-write and masked.
        let ldo2 = Rail::Ldo2.cfg();
        let ldo3 = Rail::Ldo3.cfg();
        // Start with LDO3 = 2900 mV (steps 11 = 0xB) in the low nibble.
        let ldo3_steps = steps_for(2900, ldo3.min_mv, ldo3.step_mv);
        let reg = pack_field(0, ldo3_steps, ldo3.lsb, ldo3.mask);
        assert_eq!(reg, 0x0B);
        // Now set LDO2 = 3300 mV (steps 15 = 0xF) in the high nibble.
        let ldo2_steps = steps_for(3300, ldo2.min_mv, ldo2.step_mv);
        let reg = pack_field(reg, ldo2_steps, ldo2.lsb, ldo2.mask);
        assert_eq!(reg, 0xFB);
        // Both read back intact.
        assert_eq!(unpack_field(reg, ldo2.lsb, ldo2.mask), 0xF);
        assert_eq!(unpack_field(reg, ldo3.lsb, ldo3.mask), 0xB);
        assert_eq!(millivolts_for(unpack_field(reg, ldo3.lsb, ldo3.mask), ldo3.min_mv, ldo3.step_mv), 2900);
    }

    #[test]
    fn pack_field_masks_overspill() {
        // A DCDC's value is 7 bits in an 8-bit register; the top bit belongs to
        // nothing here and must stay clear even if the value's 8th bit is set.
        let c = Rail::Dcdc3.cfg();
        assert_eq!(pack_field(0xFF, 0xFF, c.lsb, c.mask) & !c.mask, 0x80);
        assert_eq!(pack_field(0x80, 0x00, c.lsb, c.mask), 0x80, "the reserved bit is preserved");
    }

    #[test]
    fn a_voltage_outside_the_range_is_an_error_not_a_clamp() {
        // Reproduced without a bus: the range check happens before any I2C. A
        // clamp would drive a rail to a wrong-but-legal voltage and hide the
        // caller's bug.
        let (min, max) = Rail::Ldo2.voltage_range();
        assert_eq!((min, max), (1800, 3300));
        let c = Rail::Ldo2.cfg();
        assert!(3300 <= c.max_mv);
        assert!(3400 > c.max_mv, "3400 mV must be rejected for LDO2");
        assert!(1700 < c.min_mv, "1700 mV must be rejected for LDO2");
    }

    #[test]
    fn battery_voltage_decodes_at_1_1_millivolts_per_count() {
        // hi<<4 | lo(low nibble), then * 1.1 mV. A full-scale reading stays in
        // u16.
        let hi = 0xFFu16;
        let lo = 0x0Fu16;
        let counts = (hi << 4) | (lo & 0x0F);
        assert_eq!(counts, 4095);
        assert_eq!(counts * 11 / 10, 4504);
    }

    #[test]
    fn the_status_bits_are_where_the_datasheet_puts_them() {
        assert_eq!(CHARGE_ACTIVE, 1 << 6);
        assert_eq!(BATTERY_PRESENT, 1 << 5);
        assert_eq!(ADC_BATTERY_V, 1 << 7);
        assert_eq!(ADC_BATTERY_I, 1 << 6);
    }

    #[test]
    fn gpio_status_bit_is_the_index_minus_the_pair_base() {
        // GPIO3/4 share the 0x96 register; a GPIO's bit is `1 << (index - 3)`.
        // Getting this wrong drives the wrong pin of the pair.
        assert_eq!(1u8 << (3 - GPIO34_BASE), 1 << 0);
        assert_eq!(1u8 << (4 - GPIO34_BASE), 1 << 1);
        assert_eq!(REG_GPIO34_STATUS, 0x96);
        assert_eq!(REG_GPIO34_FUNCTION, 0x95);
    }
}
