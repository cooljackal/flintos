// SPDX-License-Identifier: Apache-2.0

//! SAR ADC facts shared by ADC1, ADC2 and any future SAR user.
//!
//! Only the pieces that are genuinely identical across the SAR channels live
//! here: the input-attenuation encoding and the 12-bit full-scale count. They
//! were previously declared once in `esp32-adc` and again, identically, in
//! `esp32-adc2` — two definitions that had to agree by hand.
//!
//! The per-channel power-up and read sequences deliberately stay in their own
//! drivers. ADC1, ADC2 and the radio-shared SAR differ in load-bearing ways —
//! the ADC2/Wi-Fi interlock, the hall sensor on ADC1, different `MEAS_START`
//! and `ATTEN` register instances — that a single shared routine would only
//! have to special-case back apart, and those sequences carry a documented
//! history of silent, hardware-only bugs.

/// Input attenuation for a SAR conversion.
///
/// The input range is **not** 0–3.3 V by default. At 0 dB the ADC saturates
/// around 1.1 V; the wider ranges trade accuracy for headroom.
///
/// | Attenuation | Usable input, roughly |
/// |---|---|
/// | 0 dB | 0–1.1 V |
/// | 2.5 dB | 0–1.5 V |
/// | 6 dB | 0–2.2 V |
/// | 11 dB | 0–3.9 V, clipped by the 3.3 V supply |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attenuation {
    Db0 = 0,
    Db2_5 = 1,
    Db6 = 2,
    Db11 = 3,
}

/// Full-scale count of a 12-bit conversion.
pub const FULL_SCALE: u16 = 4095;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attenuation_encoding_matches_the_two_bit_field() {
        assert_eq!(Attenuation::Db0 as u32, 0);
        assert_eq!(Attenuation::Db2_5 as u32, 1);
        assert_eq!(Attenuation::Db6 as u32, 2);
        assert_eq!(Attenuation::Db11 as u32, 3);
    }

    #[test]
    fn full_scale_is_twelve_bits() {
        assert_eq!(FULL_SCALE, 4095);
    }
}
