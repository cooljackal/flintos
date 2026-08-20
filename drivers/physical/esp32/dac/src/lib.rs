// SPDX-License-Identifier: Apache-2.0

//! DAC: two 8-bit analog outputs on GPIO 25 and GPIO 26.
//!
//! Each channel converts a value of 0–255 to a voltage of 0 V–VDD3P3_RTC (the
//! 3.3 V rail). There are exactly two, fixed to two pins; the ESP32 has no
//! other DAC.
//!
//! # These are the same pads as ADC2
//!
//! DAC1 is GPIO 25 is ADC2 channel 8; DAC2 is GPIO 26 is ADC2 channel 9. One
//! bond pad, two functions — which is what lets a loopback test drive the DAC
//! and read it straight back on ADC2 without a wire. It also means the two
//! cannot both own a pad at once.
//!
//! # The cosine generator is on at reset
//!
//! `SENS_DAC_CW_EN1` and `SENS_DAC_CW_EN2` **default to 1**: out of reset each
//! channel is wired to the cosine-wave generator, not to its output register.
//! Write a value without clearing them and the pin carries a tone that ignores
//! the value entirely — a pin that is doing *something*, which reads like a
//! working DAC until you look at what. This driver clears them.
//!
//! # The pad has to belong to the RTC domain
//!
//! Like ADC, the DAC pad is an RTC pad. Left with the digital GPIO peripheral
//! it is driven by two masters at once; `MUX_SEL` hands it to the analog side
//! and the input buffer and pulls come off, so the DAC is the only thing on the
//! pin. The power itself is taken under software (`DAC_XPD_FORCE`) rather than
//! left to the ULP's state machine, which is not running here.
//!
//! # Register facts
//!
//! `DR_REG_RTCIO_BASE` = `0x3FF48400`, `DR_REG_SENS_BASE` = `0x3FF48800`, from
//! esp-idf `soc/soc.h`. Fields from `soc/rtc_io_reg.h` and `soc/sens_reg.h`,
//! and the sequence from `hal/dac_ll.h`.
//!
//! | Register | Address | Fields used |
//! |---|---|---|
//! | `RTC_IO_PAD_DAC1` | `0x3FF48484` | `DAC` [26:19], `XPD_DAC` 18, `MUX_SEL` 17, `FUN_SEL` [16:15], `FUN_IE` 11, `DAC_XPD_FORCE` 10, `RUE` 27, `RDE` 28 |
//! | `RTC_IO_PAD_DAC2` | `0x3FF48488` | same layout |
//! | `RTC_GPIO_ENABLE` | `0x3FF4840C` | output-enable bits, from bit 14 by RTC GPIO number |
//! | `SENS_SAR_DAC_CTRL1` | `0x3FF48898` | `SW_TONE_EN` 16 |
//! | `SENS_SAR_DAC_CTRL2` | `0x3FF4889C` | `DAC_CW_EN1` 24, `DAC_CW_EN2` 25 |

#![no_std]

const RTCIO_BASE: u32 = 0x3FF4_8400;
const SENS_BASE: u32 = 0x3FF4_8800;

/// `RTC_IO_PAD_DAC1_REG`. DAC2's is the next word up.
const PAD_DAC1: u32 = RTCIO_BASE + 0x84;

/// `RTC_GPIO_ENABLE_REG`. The output-enable bit for a pad sits at bit
/// `14 + rtc_gpio_number`; GPIO 25 is RTC GPIO 6, GPIO 26 is RTC GPIO 7.
#[allow(clippy::identity_op)]
const RTC_GPIO_ENABLE: u32 = RTCIO_BASE + 0x0C;
const RTC_GPIO_SHIFT: u32 = 14;

/// `SENS_SAR_DAC_CTRL1_REG` and its `SW_TONE_EN`, the global tone switch.
const SAR_DAC_CTRL1: u32 = SENS_BASE + 0x98;
const SW_TONE_EN: u32 = 1 << 16;

/// `SENS_SAR_DAC_CTRL2_REG`. `DAC_CW_EN1` (24) and `DAC_CW_EN2` (25) each tie a
/// channel to the cosine generator; **both default to 1**.
const SAR_DAC_CTRL2: u32 = SENS_BASE + 0x9C;
const DAC_CW_EN1_SHIFT: u32 = 24;

// PDAC pad fields, LSB numbering, from `rtc_io_reg.h`.
const PDAC_DAC_SHIFT: u32 = 19;
const PDAC_DAC_MASK: u32 = 0xFF;
const PDAC_XPD_DAC: u32 = 1 << 18;
const PDAC_MUX_SEL: u32 = 1 << 17;
/// `FUN_SEL` [16:15]. 0 selects the RTC function; the others are reserved.
const PDAC_FUN_SEL_MASK: u32 = 0x3 << 15;
/// `FUN_IE` (11), the digital input buffer — off for an analog pad.
const PDAC_FUN_IE: u32 = 1 << 11;
/// `DAC_XPD_FORCE` (10): power the DAC by software, not the ULP FSM.
const PDAC_DAC_XPD_FORCE: u32 = 1 << 10;
const PDAC_RUE: u32 = 1 << 27;
const PDAC_RDE: u32 = 1 << 28;

/// A DAC channel, named by the GPIO it drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// GPIO 25, DAC1, also ADC2 channel 8.
    Gpio25 = 0,
    /// GPIO 26, DAC2, also ADC2 channel 9.
    Gpio26 = 1,
}

impl Channel {
    /// The GPIO this channel drives.
    pub const fn gpio(self) -> u8 {
        match self {
            Channel::Gpio25 => 25,
            Channel::Gpio26 => 26,
        }
    }

    /// The channel that drives `gpio`, if any.
    pub const fn from_gpio(gpio: u8) -> Option<Self> {
        Some(match gpio {
            25 => Channel::Gpio25,
            26 => Channel::Gpio26,
            _ => return None,
        })
    }

    const fn index(self) -> u32 {
        self as u32
    }

    /// This channel's `RTC_IO_PAD_DACn` register.
    const fn pad_reg(self) -> u32 {
        PAD_DAC1 + self.index() * 4
    }

    /// This channel's cosine-generator enable bit in `SAR_DAC_CTRL2`.
    const fn cw_en(self) -> u32 {
        1 << (DAC_CW_EN1_SHIFT + self.index())
    }

    /// This channel's output-enable bit in `RTC_GPIO_ENABLE`. GPIO 25 is RTC
    /// GPIO 6, GPIO 26 is RTC GPIO 7.
    const fn rtc_output_bit(self) -> u32 {
        1 << (RTC_GPIO_SHIFT + 6 + self.index())
    }
}

/// The DAC, taken as a whole. Its two channels share the tone switch and the
/// cosine-generator register, so one owner keeps them consistent.
pub struct Dac {
    _private: (),
}

impl Dac {
    /// Take the DAC and disable the cosine-wave generator, so a channel follows
    /// its output register rather than emitting a tone.
    ///
    /// # Safety
    /// Takes exclusive ownership of the DAC control registers.
    pub unsafe fn new() -> Self {
        // The global tone switch, off. Per-channel `DAC_CW_EN` is cleared as
        // each channel is driven, in `output`.
        let ctrl1 = SAR_DAC_CTRL1 as *mut u32;
        ctrl1.write_volatile(ctrl1.read_volatile() & !SW_TONE_EN);
        Self { _private: () }
    }

    /// Drive `ch` to `value` (0–255 spanning 0 V to the 3.3 V rail).
    ///
    /// Routes the pad to the RTC domain, powers the channel, disconnects it
    /// from the cosine generator, and writes the value.
    ///
    /// # Safety
    /// Writes the pad register and the shared cosine-generator register.
    pub unsafe fn output(&self, ch: Channel, value: u8) {
        // Route the pad to the analog domain and float it: input buffer off,
        // pulls off, RTC function. Otherwise the digital GPIO peripheral and
        // the DAC drive the same pin.
        let pad = ch.pad_reg() as *mut u32;
        let mut p = pad.read_volatile();
        p |= PDAC_MUX_SEL;
        p &= !PDAC_FUN_SEL_MASK;
        p &= !PDAC_FUN_IE;
        p &= !(PDAC_RUE | PDAC_RDE);
        // Power the DAC under software control.
        p |= PDAC_DAC_XPD_FORCE | PDAC_XPD_DAC;
        pad.write_volatile(p);

        // Turn the pad's output driver off, so the RTC GPIO peripheral is not
        // fighting the analog output.
        let en = RTC_GPIO_ENABLE as *mut u32;
        en.write_volatile(en.read_volatile() & !ch.rtc_output_bit());

        // Disconnect this channel from the cosine generator before trusting the
        // value field — it is tied to it at reset.
        let ctrl2 = SAR_DAC_CTRL2 as *mut u32;
        ctrl2.write_volatile(ctrl2.read_volatile() & !ch.cw_en());

        // Write the value.
        let mut p = pad.read_volatile();
        p &= !(PDAC_DAC_MASK << PDAC_DAC_SHIFT);
        p |= (value as u32) << PDAC_DAC_SHIFT;
        pad.write_volatile(p);
    }

    /// Power `ch` down and stop driving the pin.
    ///
    /// # Safety
    /// Writes the pad register.
    pub unsafe fn power_down(&self, ch: Channel) {
        let pad = ch.pad_reg() as *mut u32;
        pad.write_volatile(pad.read_volatile() & !(PDAC_DAC_XPD_FORCE | PDAC_XPD_DAC));
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_addresses_match_the_headers() {
        // `RTC_IO_PAD_DAC1_REG` = base + 0x84, `PAD_DAC2` the next word.
        assert_eq!(PAD_DAC1, 0x3FF4_8484);
        assert_eq!(Channel::Gpio26.pad_reg(), 0x3FF4_8488);
        // `SENS_SAR_DAC_CTRL1/2` = base + 0x98/0x9C.
        assert_eq!(SAR_DAC_CTRL1, 0x3FF4_8898);
        assert_eq!(SAR_DAC_CTRL2, 0x3FF4_889C);
        assert_eq!(RTC_GPIO_ENABLE, 0x3FF4_840C);
    }

    #[test]
    fn the_two_channels_are_gpio_25_and_26_not_pin_order_confusable() {
        assert_eq!(Channel::Gpio25.gpio(), 25);
        assert_eq!(Channel::Gpio26.gpio(), 26);
        assert_eq!(Channel::Gpio25.index(), 0);
        assert_eq!(Channel::Gpio26.index(), 1);
        assert_eq!(Channel::from_gpio(25), Some(Channel::Gpio25));
        assert_eq!(Channel::from_gpio(26), Some(Channel::Gpio26));
        // Not the ADC1 pins, not an ordinary GPIO.
        for gpio in [0u8, 24, 27, 32, 33] {
            assert_eq!(Channel::from_gpio(gpio), None, "GPIO{gpio} was accepted");
        }
    }

    #[test]
    fn the_value_field_is_eight_bits_at_nineteen() {
        // `PDAC_DAC` [26:19]. A shift of one either way puts half the range
        // into a neighbouring field and the DAC never reaches full scale.
        assert_eq!(PDAC_DAC_SHIFT, 19);
        assert_eq!(PDAC_DAC_MASK, 0xFF);
        assert_eq!((PDAC_DAC_MASK << PDAC_DAC_SHIFT), 0x07F8_0000);
        // The value field must not overlap XPD_DAC (18) below it.
        assert_eq!((PDAC_DAC_MASK << PDAC_DAC_SHIFT) & PDAC_XPD_DAC, 0);
    }

    #[test]
    fn the_cosine_enable_bits_are_24_and_25() {
        // Both default to 1, so each must be cleared per channel or the pin
        // carries a tone instead of the value.
        assert_eq!(Channel::Gpio25.cw_en(), 1 << 24);
        assert_eq!(Channel::Gpio26.cw_en(), 1 << 25);
        assert_eq!(SW_TONE_EN, 1 << 16);
    }

    #[test]
    fn the_output_enable_bit_follows_the_rtc_gpio_number() {
        // GPIO 25 is RTC GPIO 6, GPIO 26 is RTC GPIO 7, and the enable field
        // starts at bit 14 — so bits 20 and 21, not 25 and 26.
        assert_eq!(Channel::Gpio25.rtc_output_bit(), 1 << 20);
        assert_eq!(Channel::Gpio26.rtc_output_bit(), 1 << 21);
    }

    #[test]
    fn the_pad_control_bits_do_not_collide() {
        // MUX_SEL (17) hands the pad to analog; FUN_SEL [16:15] must sit just
        // below it and not be cleared by MUX_SEL's mask.
        assert_eq!(PDAC_MUX_SEL, 1 << 17);
        assert_eq!(PDAC_FUN_SEL_MASK, 0x3 << 15);
        assert_eq!(PDAC_FUN_SEL_MASK & PDAC_MUX_SEL, 0);
        assert_eq!(PDAC_XPD_DAC, 1 << 18);
        assert_eq!(PDAC_DAC_XPD_FORCE, 1 << 10);
        assert_eq!(PDAC_FUN_IE, 1 << 11);
        assert_eq!(PDAC_RUE, 1 << 27);
        assert_eq!(PDAC_RDE, 1 << 28);
        assert_eq!(PDAC_RUE & PDAC_RDE, 0);
    }
}
