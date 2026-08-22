// SPDX-License-Identifier: Apache-2.0

//! ADC2: ten channels of analog input, sharing their SAR with the Wi-Fi radio.
//!
//! # Why this is a separate crate from ADC1
//!
//! ADC2's converter, SAR2, is the same block the Wi-Fi PHY borrows for RF power
//! detection and calibration. While the radio owns it a software conversion
//! returns garbage or never completes. An API that hands out ADC2 channels with
//! no interlock invites a bug that appears the day someone turns on networking
//! and not before — the worst kind. So `read` **takes the radio's state as an
//! argument** and refuses with [`Adc2Error::RadioBusy`] when it is up.
//!
//! The state is passed in rather than imported deliberately: this crate must
//! not depend on the radio crate (that would be a dependency cycle, radio →
//! drivers → radio), and the radio is the only thing that knows whether it is
//! running. The caller holds both and connects them. See issue #75.
//!
//! # These channels overlap the DAC and a lot of strapping pins
//!
//! Channel 8 is GPIO 25 is DAC1; channel 9 is GPIO 26 is DAC2. That shared pad
//! is what a DAC→ADC2 loopback test uses — drive the DAC, read it back on the
//! same pin with no wire. Several other ADC2 pins are boot-strapping or JTAG
//! pins and are not free to use on a DevKitC without care.
//!
//! # Raw counts, not volts; attenuation as on ADC1
//!
//! Same as [`esp32_adc`]: readings are raw 12-bit SAR counts, not millivolts,
//! and the input range depends on the per-channel attenuation. See that crate's
//! docs for the attenuation table.
//!
//! # Register facts
//!
//! `DR_REG_SENS_BASE` = `0x3FF48800`, `DR_REG_SYSCON_BASE` = `0x3FF66000`, from
//! esp-idf `soc/soc.h`. Fields from `soc/sens_reg.h` and `soc/syscon_reg.h`;
//! the RTC-control sequence from `hal/adc_ll.h` `adc_ll_set_controller`.
//!
//! | Register | Address | Fields |
//! |---|---|---|
//! | `SAR_MEAS_WAIT2` | `0x3FF4880C` | `FORCE_XPD_SAR` [19:18], `FORCE_XPD_AMP` [17:16] |
//! | `SAR_MEAS_CTRL` | `0x3FF48810` | amplifier FSM [15:4] |
//! | `SAR_ATTEN2` | `0x3FF48838` | two bits per channel |
//! | `SAR_READ_CTRL2` | `0x3FF48890` | `SAR2_SAMPLE_BIT` [17:16], `SAR2_PWDET_FORCE` 27, `SAR2_DIG_FORCE` 28, `SAR2_DATA_INV` 29 |
//! | `SAR_MEAS_START2` | `0x3FF48894` | `EN_PAD_FORCE` 31, `EN_PAD` [30:19], `START_FORCE` 18, `START_SAR` 17, `DONE_SAR` 16, `DATA_SAR` [15:0] |
//! | `SYSCON_SARADC_CTRL` | `0x3FF66010` | `SAR2_MUX` 2 |

#![no_std]

use soc_esp32::poll;

const SENS_BASE: u32 = 0x3FF4_8800;
const SYSCON_BASE: u32 = 0x3FF6_6000;

#[allow(clippy::identity_op)]
const SAR_MEAS_WAIT2: u32 = SENS_BASE + 0x0C;
const SAR_MEAS_CTRL: u32 = SENS_BASE + 0x10;
const SAR_ATTEN2: u32 = SENS_BASE + 0x38;
const SAR_READ_CTRL2: u32 = SENS_BASE + 0x90;
const SAR_MEAS_START2: u32 = SENS_BASE + 0x94;
const SYSCON_SARADC_CTRL: u32 = SYSCON_BASE + 0x10;

/// `FORCE_XPD_SAR` [19:18]. 3 forces the SAR analog front end on; 0 leaves it
/// to a power controller that is not running. Shared with ADC1's SAR power —
/// this field powers both SARs' front end.
const FORCE_XPD_SAR_SHIFT: u32 = 18;
const FORCE_XPD_SAR_ON: u32 = 3;
const FORCE_XPD_SAR_MASK: u32 = 0x3;
/// `FORCE_XPD_AMP` [17:16]. 2 powers the LNA down; ADC2 does not use it.
const FORCE_XPD_AMP_SHIFT: u32 = 16;
const FORCE_XPD_AMP_OFF: u32 = 2;
const FORCE_XPD_AMP_MASK: u32 = 0x3;
/// The amplifier's FSM fields [15:4] in `SAR_MEAS_CTRL`, cleared with it.
const AMP_FSM_MASK: u32 = 0xFFF0;

/// `SAR2_SAMPLE_BIT` [17:16]. 3 selects 12-bit conversions.
const SAR2_SAMPLE_BIT_SHIFT: u32 = 16;
const SAR2_SAMPLE_BIT_12: u32 = 3;
const SAR2_SAMPLE_BIT_MASK: u32 = 0x3;
/// `SAR2_PWDET_FORCE` (27). Set hands SAR2 to the Wi-Fi power-detect path; 0
/// keeps it on the RTC controller.
const SAR2_PWDET_FORCE: u32 = 1 << 27;
/// `SAR2_DIG_FORCE` (28). 0 selects RTC/software control over the digital
/// (DMA) controller.
const SAR2_DIG_FORCE: u32 = 1 << 28;
/// `SAR2_DATA_INV` (29). Without it a rising input produces a falling count,
/// exactly as on ADC1.
const SAR2_DATA_INV: u32 = 1 << 29;

/// `SYSCON_SARADC_SAR2_MUX` (2). 1 routes SAR2 through the (RTC/digital)
/// controller rather than the power-detect block.
const SARADC_SAR2_MUX: u32 = 1 << 2;

const EN_PAD_FORCE: u32 = 1 << 31;
const EN_PAD_SHIFT: u32 = 19;
const EN_PAD_MASK: u32 = 0xFFF;
const MEAS_START_FORCE: u32 = 1 << 18;
const MEAS_START_SAR: u32 = 1 << 17;
const MEAS_DONE_SAR: u32 = 1 << 16;
const MEAS_DATA_MASK: u32 = 0xFFFF;

/// An ADC2 channel, named by the GPIO it reads. The channel number is **not**
/// the GPIO number and not pin order — channel 0 is GPIO 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Gpio4 = 0,
    Gpio0 = 1,
    Gpio2 = 2,
    Gpio15 = 3,
    Gpio13 = 4,
    Gpio12 = 5,
    Gpio14 = 6,
    /// GPIO 27. Not a DAC or strapping pin — the clean one for a reference.
    Gpio27 = 7,
    /// GPIO 25, also DAC1.
    Gpio25 = 8,
    /// GPIO 26, also DAC2.
    Gpio26 = 9,
}

impl Channel {
    /// The GPIO this channel reads.
    pub const fn gpio(self) -> u8 {
        match self {
            Channel::Gpio4 => 4,
            Channel::Gpio0 => 0,
            Channel::Gpio2 => 2,
            Channel::Gpio15 => 15,
            Channel::Gpio13 => 13,
            Channel::Gpio12 => 12,
            Channel::Gpio14 => 14,
            Channel::Gpio27 => 27,
            Channel::Gpio25 => 25,
            Channel::Gpio26 => 26,
        }
    }

    /// The channel that reads `gpio`, if any.
    pub const fn from_gpio(gpio: u8) -> Option<Self> {
        Some(match gpio {
            4 => Channel::Gpio4,
            0 => Channel::Gpio0,
            2 => Channel::Gpio2,
            15 => Channel::Gpio15,
            13 => Channel::Gpio13,
            12 => Channel::Gpio12,
            14 => Channel::Gpio14,
            27 => Channel::Gpio27,
            25 => Channel::Gpio25,
            26 => Channel::Gpio26,
            _ => return None,
        })
    }

    const fn index(self) -> u32 {
        self as u32
    }
}

/// Input attenuation and the 12-bit full-scale count, shared with ADC1 in
/// `soc_esp32::sar`.
pub use soc_esp32::sar::{Attenuation, FULL_SCALE};

/// Why a reading failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adc2Error {
    /// The SAR never reported the conversion done.
    Timeout,
    /// The Wi-Fi radio is up and owns SAR2. Not a fault — the reading was
    /// refused because it would have been garbage. Try again with the radio
    /// down.
    RadioBusy,
}

/// The interlock, factored out so it can be tested off-target: a conversion is
/// refused whenever the radio holds SAR2.
const fn guard(radio_up: bool) -> Result<(), Adc2Error> {
    if radio_up {
        Err(Adc2Error::RadioBusy)
    } else {
        Ok(())
    }
}

/// ADC2, taken as a whole. One instance: the channels share SAR2 and one set of
/// control registers.
pub struct Adc2 {
    _private: (),
}

impl Adc2 {
    /// Take ADC2 under RTC/software control at 12 bits.
    ///
    /// This does not itself check the radio — creating the handle is harmless;
    /// [`Adc2::read`] is where a conversion is refused.
    ///
    /// # Safety
    /// Takes exclusive ownership of the SAR2 control registers.
    pub unsafe fn new() -> Self {
        // Power the SAR analog front end, amplifier off. Shared with ADC1;
        // writing it again is idempotent.
        let wait2 = SAR_MEAS_WAIT2 as *mut u32;
        let mut w = wait2.read_volatile();
        w &= !(FORCE_XPD_SAR_MASK << FORCE_XPD_SAR_SHIFT);
        w |= FORCE_XPD_SAR_ON << FORCE_XPD_SAR_SHIFT;
        w &= !(FORCE_XPD_AMP_MASK << FORCE_XPD_AMP_SHIFT);
        w |= FORCE_XPD_AMP_OFF << FORCE_XPD_AMP_SHIFT;
        wait2.write_volatile(w);

        let ctrl = SAR_MEAS_CTRL as *mut u32;
        ctrl.write_volatile(ctrl.read_volatile() & !AMP_FSM_MASK);

        // RTC/software control: 12-bit, not the digital controller, not the
        // power-detect path, and un-invert the result.
        let read2 = SAR_READ_CTRL2 as *mut u32;
        let mut v = read2.read_volatile();
        v &= !(SAR2_SAMPLE_BIT_MASK << SAR2_SAMPLE_BIT_SHIFT);
        v |= SAR2_SAMPLE_BIT_12 << SAR2_SAMPLE_BIT_SHIFT;
        v &= !SAR2_DIG_FORCE;
        v &= !SAR2_PWDET_FORCE;
        v |= SAR2_DATA_INV;
        read2.write_volatile(v);

        // Route SAR2 through the controller rather than the power-detect block.
        let syscon = SYSCON_SARADC_CTRL as *mut u32;
        syscon.write_volatile(syscon.read_volatile() | SARADC_SAR2_MUX);

        // Take the pad selector and start strobe by software.
        let start = SAR_MEAS_START2 as *mut u32;
        start.write_volatile(start.read_volatile() | EN_PAD_FORCE | MEAS_START_FORCE);

        Self { _private: () }
    }

    /// Set a channel's input attenuation.
    ///
    /// # Safety
    /// Read-modify-writes a register shared by all ten channels.
    pub unsafe fn set_attenuation(&self, ch: Channel, atten: Attenuation) {
        let r = SAR_ATTEN2 as *mut u32;
        let shift = ch.index() * 2;
        let v = (r.read_volatile() & !(0x3 << shift)) | ((atten as u32) << shift);
        r.write_volatile(v);
    }

    /// Convert one sample from `ch`, unless the radio holds SAR2.
    ///
    /// `radio_up` is the caller's knowledge of whether the Wi-Fi radio is
    /// running. Passing `true` returns [`Adc2Error::RadioBusy`] without touching
    /// the converter, because the reading would be garbage.
    ///
    /// # Safety
    /// Drives the SAR2 registers. Requires that this pad be routed to the RTC
    /// analog domain first — for GPIO 25/26 the DAC does that; for the others
    /// the caller must.
    pub unsafe fn read(&self, ch: Channel, radio_up: bool) -> Result<u16, Adc2Error> {
        guard(radio_up)?;

        let start = SAR_MEAS_START2 as *mut u32;

        // Select the pad, and clear the start strobe so the write below is a
        // genuine rising edge.
        let base = (start.read_volatile() & !(EN_PAD_MASK << EN_PAD_SHIFT)) & !MEAS_START_SAR;
        start.write_volatile(base | ((1u32 << ch.index()) << EN_PAD_SHIFT));
        start.write_volatile(base | ((1u32 << ch.index()) << EN_PAD_SHIFT) | MEAS_START_SAR);

        poll::until(
            || unsafe { start.read_volatile() & MEAS_DONE_SAR != 0 },
            poll::DEFAULT_SPINS,
        )
        .map_err(|_| Adc2Error::Timeout)?;
        Ok((start.read_volatile() & MEAS_DATA_MASK) as u16)
    }

    /// Average `n` conversions.
    ///
    /// # Safety
    /// Same as [`Adc2::read`].
    pub unsafe fn read_averaged(
        &self,
        ch: Channel,
        n: u16,
        radio_up: bool,
    ) -> Result<u16, Adc2Error> {
        if n == 0 {
            return self.read(ch, radio_up);
        }
        let mut total: u32 = 0;
        for _ in 0..n {
            total += self.read(ch, radio_up)? as u32;
        }
        Ok((total / n as u32) as u16)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_addresses_match_sens_and_syscon_reg_h() {
        assert_eq!(SAR_MEAS_WAIT2, 0x3FF4_880C);
        assert_eq!(SAR_MEAS_CTRL, 0x3FF4_8810);
        assert_eq!(SAR_ATTEN2, 0x3FF4_8838);
        assert_eq!(SAR_READ_CTRL2, 0x3FF4_8890);
        assert_eq!(SAR_MEAS_START2, 0x3FF4_8894);
        assert_eq!(SYSCON_SARADC_CTRL, 0x3FF6_6010);
    }

    #[test]
    fn the_radio_interlock_refuses_while_up_and_allows_while_down() {
        // The one piece of logic that can be checked without a SAR: with the
        // radio up a read is refused before it touches hardware; with it down
        // the guard is transparent.
        assert_eq!(guard(true), Err(Adc2Error::RadioBusy));
        assert_eq!(guard(false), Ok(()));
    }

    #[test]
    fn channel_numbering_is_not_the_gpio_number() {
        // Channel 0 is GPIO 4, channel 8 is GPIO 25. Assuming channel == GPIO
        // reads a different pin while still returning a plausible number.
        assert_eq!(Channel::Gpio4.index(), 0);
        assert_eq!(Channel::Gpio27.index(), 7);
        assert_eq!(Channel::Gpio25.index(), 8);
        assert_eq!(Channel::Gpio26.index(), 9);
    }

    #[test]
    fn the_dac_pins_are_channels_8_and_9() {
        // The loopback depends on this exact overlap: DAC1/GPIO25 is channel 8,
        // DAC2/GPIO26 is channel 9. A wrong channel reads the wrong pad and the
        // loopback silently measures nothing.
        assert_eq!(Channel::from_gpio(25), Some(Channel::Gpio25));
        assert_eq!(Channel::from_gpio(26), Some(Channel::Gpio26));
        assert_eq!(Channel::Gpio25.gpio(), 25);
        assert_eq!(Channel::Gpio26.gpio(), 26);
    }

    #[test]
    fn gpio_lookup_round_trips_and_rejects_adc1_pins() {
        for gpio in [4u8, 0, 2, 15, 13, 12, 14, 27, 25, 26] {
            let ch = Channel::from_gpio(gpio).expect("ADC2 channel");
            assert_eq!(ch.gpio(), gpio);
        }
        // ADC1's pins and ordinary GPIOs must not resolve as ADC2 channels.
        for gpio in [32u8, 33, 34, 35, 36, 39, 1, 5, 40] {
            assert_eq!(Channel::from_gpio(gpio), None, "GPIO{gpio} was accepted");
        }
    }

    #[test]
    fn twelve_bit_conversions_are_encoded_as_three() {
        assert_eq!(SAR2_SAMPLE_BIT_12, 3);
        assert_eq!(SAR2_SAMPLE_BIT_12 & SAR2_SAMPLE_BIT_MASK, SAR2_SAMPLE_BIT_12);
        assert_eq!(FULL_SCALE, 4095);
    }

    #[test]
    fn the_control_bits_sit_where_the_header_says_and_do_not_overlap() {
        // PWDET_FORCE 27, DIG_FORCE 28, DATA_INV 29 — three adjacent bits in
        // READ_CTRL2. Conflating any two sends SAR2 to the wrong controller or
        // inverts the result.
        assert_eq!(SAR2_PWDET_FORCE, 1 << 27);
        assert_eq!(SAR2_DIG_FORCE, 1 << 28);
        assert_eq!(SAR2_DATA_INV, 1 << 29);
        assert_eq!(SARADC_SAR2_MUX, 1 << 2);
    }

    #[test]
    fn the_data_and_done_fields_do_not_overlap() {
        assert_eq!(MEAS_DATA_MASK, 0xFFFF);
        assert_eq!(MEAS_DONE_SAR, 1 << 16);
        assert_eq!(MEAS_DATA_MASK & MEAS_DONE_SAR, 0);
        // EN_PAD is [30:19]: twelve channels, one-hot. Channel 9 must land on
        // bit 28, inside the field and clear of START_FORCE at 18.
        assert_eq!(EN_PAD_SHIFT, 19);
        assert_eq!((1u32 << 9) << EN_PAD_SHIFT, 1 << 28);
        assert_eq!((EN_PAD_MASK << EN_PAD_SHIFT) & MEAS_START_FORCE, 0);
    }

    #[test]
    fn the_shared_sar_power_field_matches_adc1() {
        // FORCE_XPD_SAR [19:18] is the same field ADC1 sets — powering the SAR
        // front end. Getting its position wrong leaves conversions returning a
        // constant whatever the pin does.
        assert_eq!(FORCE_XPD_SAR_SHIFT, 18);
        assert_eq!(FORCE_XPD_SAR_ON & FORCE_XPD_SAR_MASK, FORCE_XPD_SAR_ON);
        assert_eq!(FORCE_XPD_AMP_SHIFT, 16);
        assert_eq!(AMP_FSM_MASK, 0xFFF0);
    }
}
