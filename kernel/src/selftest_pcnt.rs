// SPDX-License-Identifier: Apache-2.0

//! PCNT (pulse counter) self-test. Included by [`crate::selftest`].
//!
//! No encoder, no wire: the test *is* the encoder. It drives one free pad in
//! software through the GPIO matrix, routes that same pad into a PCNT unit's
//! signal input, and ties the unit's control input to a matrix constant to pick
//! the direction. Then it toggles the pad a known number of times and checks the
//! signed count.
//!
//! Three properties, on one pin:
//!
//! - **Counts up.** Control tied high, N rising edges toggled → count `== N`.
//! - **Counts down with direction.** Control tied low (inverts the mode), N more
//!   rising edges → count `== -N`. A negative reading is the direction bit.
//! - **The glitch filter takes effect.** With the filter set to its maximum
//!   width, the same edges toggled back-to-back (each pulse far shorter than the
//!   filter window) are rejected → the count does not move.
//!
//! Needs a pad nothing else drives — `board::active::PCNT_LOOPBACK_GPIO`. A
//! board that declares `None` skips this.

use super::Check;

/// Edges to toggle per direction. Small, so the free-wrapping 16-bit counter
/// never approaches its limits and the count is exactly the edge total.
#[cfg(target_os = "none")]
const EDGES: i16 = 17;

/// Drive the pad in software, route it into PCNT, and check up/down counting
/// and the glitch filter.
#[cfg(target_os = "none")]
pub(crate) fn pcnt_counts_edges_with_direction_and_filter(pin: u8) -> Check {
    use esp32_gpio::{Esp32Gpio, PinLevel, PinMode};
    use esp32_pcnt::{ChannelMode, Filter, PcntUnit};
    use soc_esp32::addr::GPIO_BASE;
    use soc_esp32::gpio_matrix;

    // Unit 0, the classic "rising edge steps, control level picks direction"
    // encoder mode, filter off to start.
    let pcnt = unsafe { PcntUnit::new(0, ChannelMode::UP_DOWN_ON_RISING, Filter::Off) }
        .map_err(|_| "the PCNT unit would not configure")?;

    // Software-drive the pad: matrix output = GPIO_OUT register, output driver
    // enabled, and route the same pad into PCNT's signal input.
    let gpio = unsafe { Esp32Gpio::new(GPIO_BASE) };
    gpio.set_mode(pin, PinMode::Output).map_err(|_| "the PCNT pulse pad would not become an output")?;
    gpio.write(pin, PinLevel::Low).map_err(|_| "the PCNT pulse pad would not drive low")?;
    unsafe {
        gpio_matrix::connect_output(pin, gpio_matrix::SIG_GPIO_OUT, true, false)
            .map_err(|_| "the PCNT pulse pad would not route to GPIO_OUT")?;
        pcnt.route_signal(pin).map_err(|_| "the PCNT signal input would not route")?;
    }

    // ── Up ──────────────────────────────────────────────────────────────────
    unsafe { pcnt.route_control_level(true) }.map_err(|_| "the PCNT control input would not tie high")?;
    pcnt.clear();
    pcnt.resume();
    toggle(&gpio, pin, EDGES, WIDE);
    let up = pcnt.count();

    // ── Down ────────────────────────────────────────────────────────────────
    pcnt.pause();
    pcnt.clear();
    unsafe { pcnt.route_control_level(false) }.map_err(|_| "the PCNT control input would not tie low")?;
    pcnt.resume();
    toggle(&gpio, pin, EDGES, WIDE);
    let down = pcnt.count();

    // ── Filter ──────────────────────────────────────────────────────────────
    // Widest filter, then toggle back-to-back so every pulse is far narrower
    // than the window and must be discarded. Control is still low; without the
    // filter this would read -EDGES like the down pass.
    pcnt.pause();
    pcnt.clear();
    pcnt.set_filter(Filter::Cycles(esp32_pcnt::MAX_FILTER_THRES));
    pcnt.resume();
    toggle(&gpio, pin, EDGES, NARROW);
    let filtered = pcnt.count();
    pcnt.pause();

    {
        use crate::debug::fault::{raw_dec, raw_print};
        raw_print("[FLINT]   pcnt up=");
        raw_dec(up as i32 as u32);
        raw_print(" down=");
        raw_dec(down as i32 as u32);
        raw_print(" filtered=");
        raw_dec(filtered as i32 as u32);
        raw_print("\r\n");
    }

    if up != EDGES {
        return Err("PCNT did not count up by the number of edges toggled");
    }
    if down != -EDGES {
        return Err("PCNT did not count down when the control line was flipped");
    }
    if filtered != 0 {
        return Err("the glitch filter did not reject sub-threshold pulses");
    }
    Ok(())
}

/// A pad-settling delay comfortably longer than the widest glitch filter
/// (`MAX_FILTER_THRES` = 1023 APB cycles), so an accepted pulse is unambiguously
/// wider than the filter window.
#[cfg(target_os = "none")]
const WIDE: u32 = 4000;
/// No delay: pulses this narrow are a handful of cycles, well under the filter
/// window, so the filter must drop them.
#[cfg(target_os = "none")]
const NARROW: u32 = 0;

/// Toggle `count` rising edges on `pin`, holding each level for `hold` CPU
/// cycles. Starts and ends low, so each iteration is exactly one rising edge.
#[cfg(target_os = "none")]
fn toggle(gpio: &esp32_gpio::Esp32Gpio, pin: u8, count: i16, hold: u32) {
    use esp32_gpio::PinLevel;
    for _ in 0..count {
        let _ = gpio.write(pin, PinLevel::High);
        super::spin_cycles(hold);
        let _ = gpio.write(pin, PinLevel::Low);
        super::spin_cycles(hold);
    }
}

// Host stand-in: there is no PCNT peripheral or pad to drive.
#[cfg(not(target_os = "none"))]
pub(crate) fn pcnt_counts_edges_with_direction_and_filter(_pin: u8) -> Check {
    Ok(())
}
