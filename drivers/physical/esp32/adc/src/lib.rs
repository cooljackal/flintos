// SPDX-License-Identifier: Apache-2.0

//! ADC1: eight channels of analog input on GPIO 32–39.
//!
//! # ADC1 only, and that is deliberate
//!
//! ADC2 shares its SAR with the Wi-Fi radio and is unusable whenever the radio
//! is up. An API that hands out ADC2 channels invites a bug that appears the
//! day someone turns on networking and not before, which is the worst kind.
//! This crate does not expose ADC2 at all. If it ever does, it must refuse
//! while the radio is running.
//!
//! # Raw counts are not volts
//!
//! Every reading here is a raw SAR count. The ESP32's ADC is not linear and
//! its gain and offset vary per chip; Espressif burns per-chip calibration
//! into eFuse for exactly that reason. Converting counts to millivolts without
//! it is off by enough to matter — tens of millivolts, more at the ends of the
//! range.
//!
//! So this returns counts, and says so in the type. `to_millivolts` is
//! deliberately absent rather than approximate: a function that returns a
//! number in volts is trusted as volts.
//!
//! # Attenuation
//!
//! The input range is **not** 0–3.3 V by default. At 0 dB the ADC saturates
//! around 1.1 V; the wider ranges trade accuracy for headroom. Picking the
//! wrong one gives a reading pinned at full scale, which reads like a shorted
//! input rather than a configuration mistake.
//!
//! | Attenuation | Usable input, roughly |
//! |---|---|
//! | 0 dB | 0–1.1 V |
//! | 2.5 dB | 0–1.5 V |
//! | 6 dB | 0–2.2 V |
//! | 11 dB | 0–3.9 V, clipped by the 3.3 V supply |
//!
//! # Register facts
//!
//! From esp-idf `soc/sens_reg.h`, `DR_REG_SENS_BASE` = `0x3FF48800`:
//!
//! | Register | Offset | Fields |
//! |---|---|---|
//! | `SAR_READ_CTRL` | `0x00` | `SAR1_SAMPLE_BIT` `[17:16]`, `SAR1_DIG_FORCE` 27, `SAR1_DATA_INV` 28 |
//! | `SAR_ATTEN1` | `0x34` | two bits per channel |
//! | `SAR_MEAS_START1` | `0x54` | `EN_PAD_FORCE` 31, `EN_PAD` `[30:19]`, `START_FORCE` 18, `START_SAR` 17, `DONE_SAR` 16 (RO), `DATA_SAR` `[15:0]` (RO) |

#![no_std]

use soc_esp32::poll;

/// SENS peripheral base.
const SENS_BASE: u32 = 0x3FF4_8800;

// `+ 0x00` is an identity operation and clippy says so. Kept, and silenced
// here, because these are a transcription of the technical reference manual's
// offset table: reading down the column and checking it against the datasheet
// is how a wrong address gets caught, and dropping the one zero breaks the
// alignment that makes that possible.
#[allow(clippy::identity_op)]
const SAR_READ_CTRL: u32 = SENS_BASE + 0x00;
const SAR_ATTEN1: u32 = SENS_BASE + 0x34;
const SAR_MEAS_START1: u32 = SENS_BASE + 0x54;
const SAR_MEAS_WAIT2: u32 = SENS_BASE + 0x0C;
const SAR_MEAS_CTRL: u32 = SENS_BASE + 0x10;
const SAR_TOUCH_CTRL1: u32 = SENS_BASE + 0x58;

/// `SENS_XPD_HALL_FORCE` and `SENS_HALL_PHASE_FORCE`, bits 26 and 27 of
/// `SAR_TOUCH_CTRL1`.
///
/// **The hall sensor shares ADC1's input.** Left clear, these hand its power
/// and phase to the ULP's state machine — which is not running, so the hall
/// sensor sits in whatever state reset left it, and what the SAR converts is
/// not the pad. That is a conversion that completes, returns a plausible
/// number, and reports the same number whatever the pin is doing.
const XPD_HALL_FORCE: u32 = 1 << 26;
const HALL_PHASE_FORCE: u32 = 1 << 27;

/// `RTC_IO_HALL_SENS_REG` and its `XPD_HALL`. Taking control above is only
/// half of it; this is the half that turns the thing off.
const RTC_HALL_SENS: u32 = 0x3FF4_8400 + 0x78;
const XPD_HALL: u32 = 1 << 31;

/// `SENS_FORCE_XPD_AMP` `[17:16]`. 2 powers the amplifier down.
///
/// ADC1 does not use it — it belongs to the LNA — but "not used" and "left in
/// whatever state reset chose" are different things, and esp-idf powers it
/// down explicitly before measuring.
const FORCE_XPD_AMP_SHIFT: u32 = 16;
const FORCE_XPD_AMP_OFF: u32 = 2;
const FORCE_XPD_AMP_MASK: u32 = 0x3;

/// The amplifier's FSM fields in `SAR_MEAS_CTRL`: `AMP_RST_FB_FSM` `[7:4]`,
/// `AMP_SHORT_REF_FSM` `[11:8]`, `AMP_SHORT_REF_GND_FSM` `[15:12]`. All cleared
/// with the amplifier, since the FSM only drives it.
const AMP_FSM_MASK: u32 = 0xFFF0;

/// `SENS_FORCE_XPD_SAR` `[19:18]`. 3 forces the SAR's analog front end powered
/// on; 0 leaves it to a power controller that is not running here.
///
/// Without this, conversions still *complete* — `DONE_SAR` sets and a number
/// comes back — and the number is roughly constant whatever the pin is doing.
/// Which is exactly how this presented: 612 with the pad pulled up, 482 pulled
/// down, when it should have been most of the 0..4095 range.
const FORCE_XPD_SAR_SHIFT: u32 = 18;
const FORCE_XPD_SAR_ON: u32 = 3;
const FORCE_XPD_SAR_MASK: u32 = 0x3;

/// `SENS_SAR1_SAMPLE_BIT` `[17:16]`. 3 selects 12-bit conversions.
const SAMPLE_BIT_SHIFT: u32 = 16;
const SAMPLE_BIT_12: u32 = 3;
const SAMPLE_BIT_MASK: u32 = 0x3;

/// `SENS_SAR1_DIG_FORCE`. Clear to take the SAR by software rather than
/// leaving it to the digital controller (which serves the DMA-driven modes).
const SAR1_DIG_FORCE: u32 = 1 << 27;

/// `SENS_SAR1_DATA_INV`.
///
/// The raw result comes back **inverted** unless this is set. Nothing about
/// that is guessable: without it a rising input produces a falling count, and
/// the reading still looks like a plausible number the whole time.
const SAR1_DATA_INV: u32 = 1 << 28;

const EN_PAD_FORCE: u32 = 1 << 31;
const EN_PAD_SHIFT: u32 = 19;
const EN_PAD_MASK: u32 = 0xFFF;
const MEAS_START_FORCE: u32 = 1 << 18;
const MEAS_START_SAR: u32 = 1 << 17;
const MEAS_DONE_SAR: u32 = 1 << 16;
const MEAS_DATA_MASK: u32 = 0xFFFF;

/// An ADC1 channel, named by the GPIO it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// GPIO 36, labelled SENSOR_VP.
    Gpio36 = 0,
    /// GPIO 37. Not bonded out on most modules.
    Gpio37 = 1,
    /// GPIO 38. Not bonded out on most modules.
    Gpio38 = 2,
    /// GPIO 39, labelled SENSOR_VN.
    Gpio39 = 3,
    /// GPIO 32.
    Gpio32 = 4,
    /// GPIO 33.
    Gpio33 = 5,
    /// GPIO 34. Input only.
    Gpio34 = 6,
    /// GPIO 35. Input only.
    Gpio35 = 7,
}

impl Channel {
    /// The GPIO this channel reads.
    pub const fn gpio(self) -> u8 {
        match self {
            Channel::Gpio36 => 36,
            Channel::Gpio37 => 37,
            Channel::Gpio38 => 38,
            Channel::Gpio39 => 39,
            Channel::Gpio32 => 32,
            Channel::Gpio33 => 33,
            Channel::Gpio34 => 34,
            Channel::Gpio35 => 35,
        }
    }

    /// The channel that reads `gpio`, if any.
    pub const fn from_gpio(gpio: u8) -> Option<Self> {
        Some(match gpio {
            36 => Channel::Gpio36,
            37 => Channel::Gpio37,
            38 => Channel::Gpio38,
            39 => Channel::Gpio39,
            32 => Channel::Gpio32,
            33 => Channel::Gpio33,
            34 => Channel::Gpio34,
            35 => Channel::Gpio35,
            _ => return None,
        })
    }

    const fn index(self) -> u32 {
        self as u32
    }
}

/// Input attenuation and the 12-bit full-scale count, shared with ADC2 in
/// `soc_esp32::sar`. See the table in the module docs.
pub use soc_esp32::sar::{Attenuation, FULL_SCALE};

/// Why a reading or a pad change failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdcError {
    /// The SAR never reported the conversion done.
    Timeout,
    /// This channel's pad has no RTCIO pull. GPIO 34–39 are input-only sensor
    /// pads with no pull resistors at all — not an oversight in this driver.
    NoPullOnThisPad,
}

/// What the pad's own resistor does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pull {
    None,
    Up,
    Down,
}

// ── The pad ─────────────────────────────────────────────────────────────────
//
// ADC1's pins are RTC pads, and that is the part that catches people. Their
// pull resistors are **not** the IO_MUX ones: setting a digital pull on GPIO 33
// moves the reading by a few counts of leakage and nothing more. The pulls that
// work live in RTCIO, and so does the mux that decides whether the pad belongs
// to the digital peripheral or to the RTC domain.
//
// Layout from esp-idf `soc/rtc_io_struct.h`, `touch_pad[n]`, LSB first:
// `fun_ie` 13, `mux_sel` 19 (1 = digital, 0 = RTC), `rue` 27, `rde` 28.

const RTCIO_BASE: u32 = 0x3FF4_8400;

/// `touch_pad[0]`. `RTC_IO_TOUCH_PAD8_REG` is `0xb4`, which puts the array
/// base at `0x94` — worth deriving rather than trusting, hence the test.
const TOUCH_PAD0: u32 = RTCIO_BASE + 0x94;

const PAD_MUX_SEL: u32 = 1 << 19;
/// `fun_sel` `[18:17]`. **0 selects the RTC function**; 1, 2 and 3 are reserved.
///
/// Never written until now, and `mux_sel` alone is not enough:
/// `rtcio_ll_function_select` sets the mux *and* forces this field to
/// `RTCIO_LL_PIN_FUNC`, which is 0. A pad routed to the RTC domain but left
/// pointing at a reserved function is connected to nothing in particular.
const PAD_FUN_SEL_MASK: u32 = 0x3 << 17;
/// `fun_ie` (13), the pad's digital input buffer.
///
/// Cleared deliberately, and it would not stay set anyway: an analog pad has
/// no digital input path, so this reads back as zero however it is written.
/// Worth naming rather than ignoring, because a diagnostic built on
/// `RTC_GPIO_IN` reports zero forever and looks like a dead pin.
const PAD_FUN_IE: u32 = 1 << 13;
/// `xpd` (20) powers the pad's touch sensor, `start` (22) runs it. Both belong
/// to the touch peripheral, which shares this pin — and a touch sensor that is
/// powered discharges the pad it is measuring, so an ADC reading taken beside
/// it is of a pin being actively pulled down.
const PAD_TOUCH_XPD: u32 = 1 << 20;
const PAD_TOUCH_START: u32 = 1 << 22;
const PAD_RUE: u32 = 1 << 27;
const PAD_RDE: u32 = 1 << 28;

/// RTC GPIO state, for [`Adc1::pad_debug`]. Indexed from **bit 14** by *RTC*
/// GPIO number, which is a third numbering scheme — see [`rtc_gpio_of`].
#[allow(clippy::identity_op)]
const RTC_GPIO_OUT: u32 = RTCIO_BASE + 0x00;
const RTC_GPIO_ENABLE: u32 = RTCIO_BASE + 0x0C;
const RTC_GPIO_IN: u32 = RTCIO_BASE + 0x24;
const RTC_GPIO_SHIFT: u32 = 14;

/// The RTC GPIO number for a channel's pin.
///
/// A third numbering, unrelated to either of the other two: ADC channel 5 is
/// GPIO 33 is RTC GPIO 8. Only the pins that can be driven are listed — 34
/// through 39 are input-only pads with no output driver at all.
const fn rtc_gpio_of(ch: Channel) -> Option<u32> {
    match ch {
        Channel::Gpio33 => Some(8),
        Channel::Gpio32 => Some(9),
        _ => None,
    }
}

/// Which touch pad a channel's GPIO is, for the two that are touch pads.
///
/// The touch pads are numbered by their own scheme: T8 is GPIO 33 and T9 is
/// GPIO 32, which is neither pin order nor channel order.
const fn touch_pad_of(ch: Channel) -> Option<u32> {
    match ch {
        Channel::Gpio33 => Some(8),
        Channel::Gpio32 => Some(9),
        _ => None,
    }
}

/// What [`Adc1::pad_debug`] found. `bit` is this pad's position in the three
/// RTC GPIO registers, so a caller can pick its pin out of them.
#[derive(Debug, Clone, Copy)]
pub struct PadDebug {
    pub pad: u32,
    pub enable: u32,
    pub out: u32,
    pub input: u32,
    pub bit: u32,
}

/// ADC1, taken as a whole.
///
/// One instance: the channels share a SAR, a sample width and one set of
/// control registers, so two owners would interleave conversions and read each
/// other's results.
pub struct Adc1 {
    _private: (),
}

impl Adc1 {
    /// Take ADC1 and put it under software control at 12 bits.
    ///
    /// # Safety
    /// Takes exclusive ownership of the ADC1 registers.
    pub unsafe fn new() -> Self {
        // Power the SAR, and power the amplifier down. See the constants.
        let wait2 = SAR_MEAS_WAIT2 as *mut u32;
        let mut w = wait2.read_volatile();
        w &= !(FORCE_XPD_SAR_MASK << FORCE_XPD_SAR_SHIFT);
        w |= FORCE_XPD_SAR_ON << FORCE_XPD_SAR_SHIFT;
        w &= !(FORCE_XPD_AMP_MASK << FORCE_XPD_AMP_SHIFT);
        w |= FORCE_XPD_AMP_OFF << FORCE_XPD_AMP_SHIFT;
        wait2.write_volatile(w);

        // The amplifier's state machine drives nothing now; leave it idle
        // rather than free-running against a powered-down block.
        let ctrl = SAR_MEAS_CTRL as *mut u32;
        ctrl.write_volatile(ctrl.read_volatile() & !AMP_FSM_MASK);

        // Take the hall sensor away from the ULP and switch it off. Without
        // this the SAR converts the hall sensor rather than the pad, and does
        // it consistently enough to look like a working ADC reading a dead pin.
        let touch1 = SAR_TOUCH_CTRL1 as *mut u32;
        touch1.write_volatile(touch1.read_volatile() | XPD_HALL_FORCE | HALL_PHASE_FORCE);
        let hall = RTC_HALL_SENS as *mut u32;
        hall.write_volatile(hall.read_volatile() & !XPD_HALL);

        let ctrl = SAR_READ_CTRL as *mut u32;
        let mut v = ctrl.read_volatile();
        v &= !(SAMPLE_BIT_MASK << SAMPLE_BIT_SHIFT);
        v |= SAMPLE_BIT_12 << SAMPLE_BIT_SHIFT;
        // Software control, not the digital controller.
        v &= !SAR1_DIG_FORCE;
        // Without this every count comes back inverted.
        v |= SAR1_DATA_INV;
        ctrl.write_volatile(v);

        // Take the pad selector and the start strobe by software too. Left
        // clear, the SAR waits for a controller that is not running and no
        // conversion ever completes.
        let start = SAR_MEAS_START1 as *mut u32;
        let mut s = start.read_volatile();
        s |= EN_PAD_FORCE | MEAS_START_FORCE;
        start.write_volatile(s);

        Self { _private: () }
    }

    /// Set a channel's input attenuation.
    ///
    /// # Safety
    /// Read-modify-writes a register shared by all eight channels.
    pub unsafe fn set_attenuation(&self, ch: Channel, atten: Attenuation) {
        let r = SAR_ATTEN1 as *mut u32;
        let shift = ch.index() * 2;
        let v = (r.read_volatile() & !(0x3 << shift)) | ((atten as u32) << shift);
        r.write_volatile(v);
    }

    /// Hand the pad to the RTC domain and set its pull.
    ///
    /// Only GPIO 32 and 33 among ADC1's pins have pulls; 34–39 are input-only
    /// sensor pads with no resistors, and asking gets
    /// [`AdcError::NoPullOnThisPad`] rather than a silent no-op.
    ///
    /// # Safety
    /// Writes an RTCIO pad register.
    pub unsafe fn set_pad_pull(&self, ch: Channel, pull: Pull) -> Result<(), AdcError> {
        let pad = touch_pad_of(ch).ok_or(AdcError::NoPullOnThisPad)?;
        let r = (TOUCH_PAD0 + pad * 4) as *mut u32;
        let mut v = r.read_volatile();
        // `mux_sel` **set** hands the pad to the RTC domain, despite
        // `rtc_io_struct.h` commenting it the other way round -- esp-idf's
        // `rtc_gpio_init` sets this bit, and the comment is wrong. Cleared, the
        // pad stays with the digital GPIO peripheral, these pulls do nothing,
        // and the SAR converts a pin it is not connected to.
        v |= PAD_MUX_SEL;
        // RTC function 0. Reserved functions are not "no function".
        v &= !PAD_FUN_SEL_MASK;
        // No digital input buffer on an analog pad.
        v &= !PAD_FUN_IE;
        // And take the touch sensor off the pad we are about to measure.
        v &= !(PAD_TOUCH_XPD | PAD_TOUCH_START);
        v &= !(PAD_RUE | PAD_RDE);
        match pull {
            Pull::None => {}
            Pull::Up => v |= PAD_RUE,
            Pull::Down => v |= PAD_RDE,
        }
        r.write_volatile(v);
        Ok(())
    }

    /// Read the pad's own register and RTCIO's output state.
    ///
    /// Kept because it is what finally explained this driver. There is no
    /// driving a pad you are also measuring: `mux_sel` puts the pad in analog
    /// mode, which bypasses the digital buffers. The output *enable* sets
    /// happily — `en` shows the bit — and `fun_ie` will not set at all, so the
    /// pin floats and `RTC_GPIO_IN` reads zero whatever you do. Two sessions
    /// went into readings taken from a pad nothing was holding.
    ///
    /// # Safety
    /// Reads RTCIO registers.
    pub unsafe fn pad_debug(&self, ch: Channel) -> PadDebug {
        let pad = touch_pad_of(ch).unwrap_or(0);
        let bit = rtc_gpio_of(ch).map(|n| 1u32 << (RTC_GPIO_SHIFT + n)).unwrap_or(0);
        PadDebug {
            pad: ((TOUCH_PAD0 + pad * 4) as *const u32).read_volatile(),
            enable: (RTC_GPIO_ENABLE as *const u32).read_volatile(),
            out: (RTC_GPIO_OUT as *const u32).read_volatile(),
            input: (RTC_GPIO_IN as *const u32).read_volatile(),
            bit,
        }
    }

    /// Convert one sample from `ch`.
    ///
    /// # Safety
    /// Drives the ADC1 registers.
    pub unsafe fn read(&self, ch: Channel) -> Result<u16, AdcError> {
        let start = SAR_MEAS_START1 as *mut u32;

        // Select the pad, and clear the start strobe so the rising edge below
        // is a real edge rather than a level that was already high.
        let base = (start.read_volatile() & !(EN_PAD_MASK << EN_PAD_SHIFT)) & !MEAS_START_SAR;
        start.write_volatile(base | ((1u32 << ch.index()) << EN_PAD_SHIFT));
        start.write_volatile(
            base | ((1u32 << ch.index()) << EN_PAD_SHIFT) | MEAS_START_SAR,
        );

        poll::until(
            || unsafe { start.read_volatile() & MEAS_DONE_SAR != 0 },
            poll::DEFAULT_SPINS,
        )
        .map_err(|_| AdcError::Timeout)?;
        Ok((start.read_volatile() & MEAS_DATA_MASK) as u16)
    }

    /// Average `n` conversions, for a reading that is not one sample of noise.
    ///
    /// # Safety
    /// Same as [`Adc1::read`].
    pub unsafe fn read_averaged(&self, ch: Channel, n: u16) -> Result<u16, AdcError> {
        if n == 0 {
            return self.read(ch);
        }
        let mut total: u32 = 0;
        for _ in 0..n {
            total += self.read(ch)? as u32;
        }
        Ok((total / n as u32) as u16)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_addresses_match_sens_reg_h() {
        assert_eq!(SENS_BASE, 0x3FF4_8800);
        assert_eq!(SAR_READ_CTRL, 0x3FF4_8800);
        assert_eq!(SAR_ATTEN1, 0x3FF4_8834);
        assert_eq!(SAR_MEAS_START1, 0x3FF4_8854);
    }

    #[test]
    fn channel_numbering_is_not_the_gpio_number() {
        // ADC1_CH0 is GPIO36 and ADC1_CH4 is GPIO32 — the channels are not in
        // pin order, and assuming they are points every reading at the wrong
        // pad while still returning plausible numbers.
        assert_eq!(Channel::Gpio36.index(), 0);
        assert_eq!(Channel::Gpio39.index(), 3);
        assert_eq!(Channel::Gpio32.index(), 4);
        assert_eq!(Channel::Gpio33.index(), 5);
        assert_eq!(Channel::Gpio35.index(), 7);
    }

    #[test]
    fn gpio_lookup_round_trips_and_rejects_the_rest() {
        for gpio in [32u8, 33, 34, 35, 36, 37, 38, 39] {
            let ch = Channel::from_gpio(gpio).expect("ADC1 channel");
            assert_eq!(ch.gpio(), gpio);
        }
        // ADC2's pins and ordinary GPIOs must not resolve. Handing back an
        // ADC1 channel for an ADC2 pin would read a different pin entirely.
        for gpio in [0u8, 2, 4, 12, 25, 26, 27, 31, 40] {
            assert_eq!(Channel::from_gpio(gpio), None, "GPIO{gpio} was accepted");
        }
    }

    #[test]
    fn attenuation_occupies_two_bits_per_channel() {
        // Eight channels, two bits each, in one 32-bit register. A shift of
        // one bit per channel would make every setting bleed into its
        // neighbour.
        assert_eq!(Channel::Gpio36.index() * 2, 0);
        assert_eq!(Channel::Gpio33.index() * 2, 10);
        assert_eq!(Channel::Gpio35.index() * 2, 14);
        assert_eq!(Attenuation::Db0 as u32, 0);
        assert_eq!(Attenuation::Db11 as u32, 3);
    }

    #[test]
    fn twelve_bit_conversions_are_encoded_as_three() {
        // `SENS_SAR1_SAMPLE_BIT` counts down from 12: 3 means 12-bit, not
        // 3-bit. Writing 12 truncates to 0 and gives 9-bit readings.
        assert_eq!(SAMPLE_BIT_12, 3);
        assert_eq!(SAMPLE_BIT_12 & SAMPLE_BIT_MASK, SAMPLE_BIT_12);
        assert_eq!(FULL_SCALE, 4095);
    }

    #[test]
    fn the_data_and_done_fields_do_not_overlap() {
        // DATA is `[15:0]` and DONE is bit 16. A 17-bit data mask would make
        // every completed conversion read as 65536 too high.
        assert_eq!(MEAS_DATA_MASK, 0xFFFF);
        assert_eq!(MEAS_DONE_SAR, 1 << 16);
        assert_eq!(MEAS_DATA_MASK & MEAS_DONE_SAR, 0);
    }

    #[test]
    fn rtc_gpio_numbering_is_a_third_scheme_again() {
        // ADC channel 5 is GPIO 33 is touch pad 8 is RTC GPIO 8. Three
        // numberings, and only the last two coincide -- by accident, for these
        // two pins, which is exactly the sort of coincidence that gets
        // hardcoded and then breaks on the next pin.
        assert_eq!(rtc_gpio_of(Channel::Gpio33), Some(8));
        assert_eq!(rtc_gpio_of(Channel::Gpio32), Some(9));
        assert_eq!(Channel::Gpio33.index(), 5);
        // Input-only pads have no driver to enable.
        for ch in [Channel::Gpio34, Channel::Gpio35, Channel::Gpio36, Channel::Gpio39] {
            assert_eq!(rtc_gpio_of(ch), None, "{ch:?} has no output driver");
        }
    }

    #[test]
    fn the_rtc_gpio_registers_are_where_the_header_says() {
        // Same reason as the declarations: the point is the column read
        // against the datasheet, so the zero stays.
        #[allow(clippy::identity_op)]
        {
            assert_eq!(RTC_GPIO_OUT, RTCIO_BASE + 0x00);
        }
        assert_eq!(RTC_GPIO_ENABLE, RTCIO_BASE + 0x0C);
        assert_eq!(RTC_GPIO_IN, RTCIO_BASE + 0x24);
        // The field starts at 14, not 0. Indexing from 0 would drive a pin
        // seventeen places away, if it exists at all.
        assert_eq!(RTC_GPIO_SHIFT, 14);
        assert_eq!(1u32 << (RTC_GPIO_SHIFT + 8), 1 << 22);
    }

    #[test]
    fn the_hall_sensor_is_taken_from_the_ulp_and_switched_off() {
        // It shares ADC1's input. Both halves are needed: taking control
        // without switching it off leaves it powered, and switching it off
        // without taking control lets the ULP turn it back on.
        assert_eq!(SAR_TOUCH_CTRL1, 0x3FF4_8858);
        assert_eq!(XPD_HALL_FORCE, 1 << 26);
        assert_eq!(HALL_PHASE_FORCE, 1 << 27);
        assert_eq!(RTC_HALL_SENS, 0x3FF4_8478);
        assert_eq!(XPD_HALL, 1 << 31);
    }

    #[test]
    fn the_amplifier_fsm_mask_covers_its_three_fields_and_nothing_else() {
        // AMP_RST_FB_FSM `[7:4]`, AMP_SHORT_REF_FSM `[11:8]`,
        // AMP_SHORT_REF_GND_FSM `[15:12]`. A mask one nibble wide either way
        // would clear a neighbouring field that is not the amplifier's.
        assert_eq!(SAR_MEAS_CTRL, 0x3FF4_8810);
        assert_eq!(AMP_FSM_MASK, 0xFFF0);
        assert_eq!(AMP_FSM_MASK & 0xF, 0, "bits 0..4 are not the amp's");
        assert_eq!(AMP_FSM_MASK >> 16, 0, "bits 16+ are not the amp's");
    }

    #[test]
    fn the_sar_power_field_is_two_bits_at_eighteen() {
        assert_eq!(SAR_MEAS_WAIT2, 0x3FF4_880C);
        assert_eq!(FORCE_XPD_SAR_SHIFT, 18);
        assert_eq!(FORCE_XPD_SAR_ON & FORCE_XPD_SAR_MASK, FORCE_XPD_SAR_ON);
    }

    #[test]
    fn the_touch_pad_array_base_lands_on_the_documented_registers() {
        // `RTC_IO_TOUCH_PAD8_REG` = base + 0xb4 and PAD9 = 0xb8. Deriving the
        // array base from those is only safe if the arithmetic agrees.
        assert_eq!(TOUCH_PAD0 + 8 * 4, RTCIO_BASE + 0xB4);
        assert_eq!(TOUCH_PAD0 + 9 * 4, RTCIO_BASE + 0xB8);
    }

    #[test]
    fn only_the_two_touch_pad_channels_have_pulls() {
        // GPIO 33 is T8 and GPIO 32 is T9 -- not pin order, not channel order.
        assert_eq!(touch_pad_of(Channel::Gpio33), Some(8));
        assert_eq!(touch_pad_of(Channel::Gpio32), Some(9));
        for ch in [
            Channel::Gpio34,
            Channel::Gpio35,
            Channel::Gpio36,
            Channel::Gpio37,
            Channel::Gpio38,
            Channel::Gpio39,
        ] {
            assert_eq!(touch_pad_of(ch), None, "{ch:?} claimed a pull it has not got");
        }
    }

    #[test]
    fn the_pad_pull_bits_are_not_the_io_mux_ones() {
        // From `rtc_io_struct.h`: rue 27, rde 28, mux_sel 19. Getting these
        // from the digital IO_MUX layout instead moves the reading by a few
        // counts of leakage, which looks like a weak pull rather than none.
        assert_eq!(PAD_RUE, 1 << 27);
        assert_eq!(PAD_RDE, 1 << 28);
        assert_eq!(PAD_MUX_SEL, 1 << 19);
        assert_eq!(PAD_FUN_SEL_MASK, 0x3 << 17);
        assert_eq!(PAD_FUN_IE, 1 << 13);
        // fun_sel must not overlap mux_sel; they are adjacent and easy to
        // conflate, and clearing one with the other's mask undoes the routing.
        assert_eq!(PAD_FUN_SEL_MASK & PAD_MUX_SEL, 0);
        assert_eq!(PAD_TOUCH_XPD, 1 << 20);
        assert_eq!(PAD_TOUCH_START, 1 << 22);
        assert_eq!(PAD_RUE & PAD_RDE, 0);
    }

    #[test]
    fn the_pad_selector_clears_the_start_strobe() {
        // `read` builds its base word by clearing both the pad field and
        // START_SAR, so the write that follows is a genuine rising edge. A
        // base that left START_SAR set would start a conversion on the old
        // pad before the new one was selected.
        let stale = EN_PAD_FORCE | MEAS_START_FORCE | MEAS_START_SAR | (0xFFF << EN_PAD_SHIFT);
        let base = (stale & !(EN_PAD_MASK << EN_PAD_SHIFT)) & !MEAS_START_SAR;
        assert_eq!(base & MEAS_START_SAR, 0, "start strobe survived");
        assert_eq!(base & (EN_PAD_MASK << EN_PAD_SHIFT), 0, "pad selection survived");
        // The two force bits must not be cleared with it — without them the
        // SAR waits on a controller that is not running.
        assert_eq!(base & EN_PAD_FORCE, EN_PAD_FORCE);
        assert_eq!(base & MEAS_START_FORCE, MEAS_START_FORCE);
    }
}
