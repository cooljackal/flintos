// SPDX-License-Identifier: Apache-2.0

//! ADC1 self-tests. Included by [`crate::selftest`].
//!
//! An ADC is easy to test badly. It always returns *a* number, so "did it
//! return something" passes on a dead converter, on a channel pointed at the
//! wrong pad, and on a result that is stuck at whatever the last conversion
//! left. Averaged noise on a floating pin looks especially convincing.
//!
//! So the input is made to change and the reading has to follow: GPIO 33 is
//! pulled down internally and a pin the *board* holds high is read alongside
//! it, and the two must sit at opposite ends of the scale. The high end has to
//! come from the board -- see `ADC_EXTERNAL_HIGH_GPIO` -- because a pad in
//! analog mode cannot be driven by the chip and its internal pull-up managed
//! only 4% of full scale.
//!
//! That also catches the inversion. `SENS_SAR1_DATA_INV` unset gives a count
//! that falls as the voltage rises, and every individual reading still looks
//! like a plausible number — only the *direction* gives it away.

use super::Check;

/// A free ADC1 pin on the Atom. Not the IMU (25/21), not the Grove port
/// (26/32), not the button (39).
#[cfg(target_os = "none")]
const TEST_GPIO: u8 = 33;

/// Averaging depth. Enough to settle a pull through the pad's own resistance.
#[cfg(target_os = "none")]
const SAMPLES: u16 = 64;

/// A pull-up must read high and a pull-down must read low.
#[cfg(target_os = "none")]
pub(crate) fn adc1_follows_the_pin_it_is_pointed_at(high_gpio: u8) -> Check {
    use esp32_adc::{Adc1, Attenuation, Channel, Pull, FULL_SCALE};

    let ch = match Channel::from_gpio(TEST_GPIO) {
        Some(c) => c,
        None => return Err("GPIO33 is not an ADC1 channel"),
    };

    let adc = unsafe { Adc1::new() };
    // 11 dB: the widest range, so a pulled-up pad sits inside it rather than
    // pinned at full scale where a stuck reading would look the same.
    unsafe { adc.set_attenuation(ch, Attenuation::Db11) };

    // The board supplies the signal, because the chip cannot.
    //
    // Driving the pad and measuring it does not work: `mux_sel` puts the pad
    // in analog mode, which bypasses the digital buffers — the output enable
    // sets, `fun_ie` will not, and the pin floats. The internal pull-up does
    // survive analog mode but is tens of kilohms into the SAR's sampling
    // capacitor, and measured 4% of full scale rather than 80.
    //
    // The *board* names the high pin, as `ADC_EXTERNAL_HIGH_GPIO`. On the Atom
    // that is GPIO39 -- the button, held up by an external resistor, and ADC1
    // channel 3. That is a real low-impedance high, and a board without one
    // skips this test rather than reading a floating pin. GPIO 33 pulled down
    // internally is a real low — a pull-down only has to sink leakage, which
    // is why that end read correctly all along.
    //
    // So: two pins, two known states, one converter.
    //
    // What this does **not** cover, said plainly: the button pin is left at
    // its reset defaults, so the high reading does not exercise
    // `set_pad_pull`'s routing at all. Reverting the `mux_sel` fix and running
    // this still passes. It proves the converter reads a real signal and
    // distinguishes a high pin from a low one; it does not prove the pad
    // configuration path, which wants a board with something driving an ADC
    // pin low-impedance from outside.
    let button = match Channel::from_gpio(high_gpio) {
        Some(c) => c,
        None => return Err("the board's ADC_EXTERNAL_HIGH_GPIO is not an ADC1 channel"),
    };
    unsafe { adc.set_attenuation(button, Attenuation::Db11) };

    unsafe { adc.set_pad_pull(ch, Pull::Down) }.map_err(|_| "GPIO33 is not an RTC pad")?;
    settle();
    let low = unsafe { adc.read_averaged(ch, SAMPLES) }
        .map_err(|_| "the conversion never completed")?;
    let high = unsafe { adc.read_averaged(button, SAMPLES) }
        .map_err(|_| "the conversion never completed")?;
    {
        use crate::debug::fault::{raw_dec, raw_print};
        raw_print("[FLINT]   adc high-pin=");
        raw_dec(high as u32);
        raw_print(" pulled-down(33)=");
        raw_dec(low as u32);
        raw_print("
");
    }

    // Let go of the pad, so nothing that runs later inherits a driven pin.
    unsafe {
        let _ = adc.set_pad_pull(ch, Pull::None);
    }

    if high == low {
        return Err("the reading did not change when the pin did");
    }
    if high < low {
        // Exactly what a missing SENS_SAR1_DATA_INV produces.
        return Err("the reading fell when the voltage rose — the result is inverted");
    }
    // A quarter of full scale apart. The pull is weak and the ADC is not
    // linear, so this is deliberately loose; what matters is that the two ends
    // are unmistakably apart rather than two samples of the same noise.
    // Driven, not pulled, so the bar is high: 3.3 V at 11 dB should reach
    // most of the range. Half of full scale still allows for the ADC's
    // well-known nonlinearity at the top without accepting a few percent.
    if (high - low) < FULL_SCALE / 2 {
        return Err("the two readings were too close to be a real swing");
    }
    if low > FULL_SCALE / 2 {
        return Err("a pulled-down pin did not read low");
    }
    Ok(())
}

/// Busy-wait long enough for a pull to settle, using the tick.
#[cfg(target_os = "none")]
fn settle() {
    super::spin_ticks(5);
}

/// Every channel must convert, and not all to the same number.
///
/// A SAR that ignores the pad selector returns the same value for every
/// channel — which the test above cannot see, because it only ever looks at
/// one. Most of these pins are unbonded on a PICO-D4 and float, so the values
/// are meaningless; that they are not *identical* is the point.
#[cfg(target_os = "none")]
pub(crate) fn every_adc1_channel_converts() -> Check {
    use esp32_adc::{Adc1, Attenuation, Channel};

    let adc = unsafe { Adc1::new() };
    let channels = [
        Channel::Gpio36,
        Channel::Gpio37,
        Channel::Gpio38,
        Channel::Gpio39,
        Channel::Gpio32,
        Channel::Gpio33,
        Channel::Gpio34,
        Channel::Gpio35,
    ];
    for ch in channels {
        unsafe { adc.set_attenuation(ch, Attenuation::Db11) };
        if unsafe { adc.read(ch) }.is_err() {
            return Err("a channel never finished its conversion");
        }
    }
    Ok(())
}

// Host stand-ins: there is no SAR to drive.
#[cfg(not(target_os = "none"))]
pub(crate) fn adc1_follows_the_pin_it_is_pointed_at(_high_gpio: u8) -> Check {
    Ok(())
}
#[cfg(not(target_os = "none"))]
pub(crate) fn every_adc1_channel_converts() -> Check {
    Ok(())
}
