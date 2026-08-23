// SPDX-License-Identifier: Apache-2.0

//! Touch-sensor self-test. Included by [`crate::selftest`].
//!
//! # Why this needs the chip, and what it can prove without a finger
//!
//! The touch controller measures a pad's capacitance by counting how many times
//! a fixed-slope charge/discharge cycle fits into a fixed measurement window.
//! There is no way for an automated test to *touch* the pad, so it cannot check
//! the one thing a button cares about — that the count drops when a finger adds
//! capacitance. What it *can* prove, and what has actually broken in bring-up,
//! is that the controller measures at all: that the FSM runs a software-triggered
//! conversion to completion and returns a count that is neither zero (the pad
//! never charged, or the readout is wrong) nor saturated (the conversion never
//! terminated), and that the count is stable across repeats (a floating,
//! untouched pad has a steady parasitic capacitance). That is the same shape as
//! the TWAI and I2S loopback tests: prove the controller, not the wire.
//!
//! Needs one free touch-capable pad the board declares
//! (`board::active::TOUCH_SELFTEST_GPIO`); a board that declares `None` skips it.

use super::Check;

/// A software-triggered touch measurement must return a plausible, stable count.
#[cfg(target_os = "none")]
pub(crate) fn touch_reads_a_stable_capacitance_count(gpio: u8) -> Check {
    use esp32_touch::{Channel, Touch};

    let ch = Channel::from_gpio(gpio)
        .ok_or("the board's touch self-test GPIO is not a touch-capable pad")?;

    let touch = unsafe { Touch::new() };

    // A first conversion. Zero means the pad never charged or the readout is
    // wrong; all-ones means the conversion never terminated within its window.
    let first = touch.read(ch)
        .map_err(|_| "the touch FSM never signalled done -- the conversion did not run")?;
    if first == 0 {
        return Err("touch count is zero -- the controller measured nothing");
    }
    if first == u16::MAX {
        return Err("touch count is saturated -- the conversion never terminated");
    }

    // Repeats on an untouched pad track its steady parasitic capacitance, so
    // consecutive counts must stay close. A wildly varying count means the
    // measurement window or the readout is racing the FSM, not sensing a pad.
    // The band is wide (an eighth of the reading) because the RC oscillator that
    // clocks the measurement is uncalibrated; it is not slack for a count that
    // swings by half.
    let mut min = first;
    let mut max = first;
    for _ in 0..4 {
        let c = touch.read(ch)
            .map_err(|_| "a repeat touch conversion timed out")?;
        if c == 0 || c == u16::MAX {
            return Err("a repeat touch conversion returned an out-of-range count");
        }
        min = min.min(c);
        max = max.max(c);
    }
    // max - min must be within first/8. Compared as u32 to avoid any overflow.
    if (max - min) as u32 * 8 > first as u32 {
        return Err("touch counts are not stable across repeats");
    }

    Ok(())
}

/// Host stand-in: there is no touch controller to drive.
#[cfg(not(target_os = "none"))]
pub(crate) fn touch_reads_a_stable_capacitance_count(_gpio: u8) -> Check {
    Ok(())
}
