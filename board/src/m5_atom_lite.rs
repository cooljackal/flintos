// SPDX-License-Identifier: Apache-2.0

//! M5Stack Atom Lite: one SK6812 on GPIO 27.
//!
//! Everything except the LED is shared with the Matrix — see
//! `m5_atom_common.rs`.

pub use super::m5_atom_common::*;

pub const BOARD_NAME: &str = "M5Stack-ATOM Lite (ESP32-PICO-D4)";

/// One LED on [`RGB_LED_GPIO`].
pub const RGB_LED_COUNT: usize = 1;

/// No panel. A single LED has no geometry to describe, and saying so with
/// `None` is what lets an application tell the two Atoms apart at compile
/// time instead of lighting one pixel of a panel and calling it done.
pub const RGB_LED_LAYOUT: Option<led_matrix::Layout> = None;

/// This board as one value; see [`crate::Board`]. One addressable LED, no IMU.
pub const BOARD: crate::Board = crate::Board {
    name: BOARD_NAME,
    imu: None,
    rgb_led: Some(crate::RgbLed { gpio: RGB_LED_GPIO, count: RGB_LED_COUNT, layout: RGB_LED_LAYOUT }),
    selftest: SELFTEST_PADS,
    console: CONSOLE,
};
