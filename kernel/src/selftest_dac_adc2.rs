// SPDX-License-Identifier: Apache-2.0

//! DAC and ADC2 self-tests, run as one loopback. Included by [`crate::selftest`].
//!
//! DAC1 is GPIO 25 is ADC2 channel 8; DAC2 is GPIO 26 is ADC2 channel 9. The
//! DAC and the ADC share the *same bond pad*, so driving the DAC and reading
//! ADC2 on the same pin needs no wire — the loopback is on-chip, through the
//! real analog path (output buffer → pad → SAR), not an internal register mux.
//!
//! This tests both drivers at once, which is the point: a DAC that outputs
//! nothing and an ADC2 that returns a constant both fail here, because the
//! reading has to *track* the value the DAC is told to produce. A single
//! reading proves neither.
//!
//! What it does **not** cover: absolute accuracy. The counts are not converted
//! to volts (see the ADC docs) and the ADC is nonlinear, so the test asserts
//! direction and a large swing, not a calibrated value. It is a loopback
//! functional test, not a metrology test.

use super::Check;

/// Averaging depth, enough to settle the SAR's sampling on the DAC's output.
#[cfg(target_os = "none")]
const SAMPLES: u16 = 32;

/// Drive each DAC channel low then high and require ADC2 to follow on the
/// shared pad.
#[cfg(target_os = "none")]
pub(crate) fn dac_drives_and_adc2_reads_it_back() -> Check {
    use esp32_adc2::{Adc2, Attenuation, Channel as A};
    use esp32_dac::{Channel as D, Dac};

    let dac = unsafe { Dac::new() };
    let adc = unsafe { Adc2::new() };

    // (DAC channel, ADC2 channel) for the two shared pads.
    for (dch, ach, label) in [(D::Gpio25, A::Gpio25, "gpio25"), (D::Gpio26, A::Gpio26, "gpio26")] {
        // 11 dB: the widest range, so 3.3 V of DAC output sits inside it rather
        // than pinned at full scale where a stuck reading would look the same.
        unsafe { adc.set_attenuation(ach, Attenuation::Db11) };

        unsafe { dac.output(dch, 0) };
        settle();
        let low = unsafe { adc.read_averaged(ach, SAMPLES, false) }
            .map_err(|_| "the ADC2 conversion never completed")?;

        unsafe { dac.output(dch, 255) };
        settle();
        let high = unsafe { adc.read_averaged(ach, SAMPLES, false) }
            .map_err(|_| "the ADC2 conversion never completed")?;

        {
            use crate::debug::fault::{raw_dec, raw_print};
            raw_print("[FLINT]   dac-adc2 ");
            raw_print(label);
            raw_print(" dac0=");
            raw_dec(low as u32);
            raw_print(" dac255=");
            raw_dec(high as u32);
            raw_print("\r\n");
        }

        // Leave the pin quiet for anything that runs after.
        unsafe { dac.power_down(dch) };

        if high <= low {
            return Err("the ADC2 reading did not rise when the DAC did");
        }
        // Loose, as on ADC1: the ADC is nonlinear and the point is an
        // unmistakable swing, not a calibrated value. Full DAC range across
        // 11 dB should still cover most of the scale.
        if (high - low) < (esp32_adc2::FULL_SCALE / 2) {
            return Err("the DAC swing barely moved the ADC2 reading");
        }
        if low > esp32_adc2::FULL_SCALE / 2 {
            return Err("a DAC output of 0 did not read low");
        }
    }
    Ok(())
}

/// A conversion must be refused while the radio owns SAR2.
///
/// The interlock is the whole reason ADC2 is separate from ADC1. Here the radio
/// is down, so the refusal is asserted by *telling* the driver it is up — the
/// caller owns that knowledge (issue #75). A driver that read anyway would hand
/// back garbage the day someone turns on Wi-Fi.
#[cfg(target_os = "none")]
pub(crate) fn adc2_refuses_a_read_while_the_radio_is_up() -> Check {
    use esp32_adc2::{Adc2, Adc2Error, Channel as A};

    let adc = unsafe { Adc2::new() };
    match unsafe { adc.read(A::Gpio27, true) } {
        Err(Adc2Error::RadioBusy) => Ok(()),
        Err(_) => Err("ADC2 read failed, but not for the radio"),
        Ok(_) => Err("ADC2 read the SAR while the radio was said to own it"),
    }
}

/// Busy-wait long enough for the DAC output and the SAR sampling to settle.
#[cfg(target_os = "none")]
fn settle() {
    super::spin_ticks(2);
}

// Host stand-ins: there is no DAC or SAR to drive.
#[cfg(not(target_os = "none"))]
pub(crate) fn dac_drives_and_adc2_reads_it_back() -> Check {
    Ok(())
}
#[cfg(not(target_os = "none"))]
pub(crate) fn adc2_refuses_a_read_while_the_radio_is_up() -> Check {
    Ok(())
}
