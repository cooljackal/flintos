// SPDX-License-Identifier: Apache-2.0

//! M5Stack Atom Matrix: a 5×5 SK6812 panel on GPIO 27.
//!
//! Everything except the panel is shared with the Lite — see
//! `m5_atom_common.rs`.

use led_matrix::{Axis, Layout, Order, Origin};
use soc_esp32::{I2cCtrl, I2cPort};
use hal::bus::{BusSpeed, I2cConfig};

pub use super::m5_atom_common::*;

pub const BOARD_NAME: &str = "M5Stack-ATOM Matrix (ESP32-PICO-D4)";

/// 25 LEDs in one chain on [`RGB_LED_GPIO`].
///
/// 25 × 24 bits is 600 RMT entries against a 64-entry block, so this board
/// cannot be driven without `Rmt::start_stream`.
pub const RGB_LED_COUNT: usize = 5 * 5;

/// How the panel is folded.
///
/// **Measured, not read off a datasheet.** Walking the chain one LED at a time
/// (`apps/examples/blink`) gave: index 0 at the bottom-right, running right-to-left
/// along the bottom row, then jumping back to the right edge one row up.
///
/// So it is *progressive*, not zigzag — the arrangement that needs a return
/// wire per row, and the less common of the two. Guessing would have got it
/// wrong, and a wrong fold lights the wrong pixel, which reads as a broken
/// panel rather than a wrong constant.
///
/// A layout is a fact about a board, in the same way a pin number is, which is
/// why it is declared here rather than shipped as a preset by `led-matrix`.
pub const RGB_LED_LAYOUT: Option<Layout> = Some(Layout::new(
    5,
    5,
    Origin::BottomRight,
    Axis::Rows,
    Order::Progressive,
));

/// The onboard IMU's I2C pins.
///
/// A *private* bus: these pins go nowhere but the IMU, and are not the Grove
/// port (`GROVE_SDA_GPIO` / `GROVE_SCL_GPIO`, GPIO 26/32).
///
/// Declared on the Matrix and not in the shared manifest because the ATOM Lite
/// has no IMU. Putting them in `m5_atom_common.rs` would say the Lite has one,
/// which is the same class of mistake as declaring an LED pin without a count.
pub const IMU_SDA_GPIO: u8 = 25;
pub const IMU_SCL_GPIO: u8 = 21;

/// The IMU's I2C address.
///
/// **The address identifies the socket, not the part.** M5Stack shipped this
/// board with an MPU6886 and later revisions with a BMI270, and both answer
/// here. Only the chip ID register tells them apart -- see `drivers/logical/bmi270`.
pub const IMU_I2C_ADDR: u8 = 0x68;

/// The IMU as an [`I2cPort`]: I2C0 at 100 kHz on the private SDA/SCL pins.
///
/// This closes what the loose `IMU_SDA_GPIO`/`IMU_SCL_GPIO`/`IMU_I2C_ADDR`
/// consts left half-declared — *which controller* the two pins belong to. The
/// pins are private to the IMU (not the Grove port), so I2C0 is free for it.
pub const IMU_PORT: I2cPort = I2cPort {
    ctrl: I2cCtrl::I2c0,
    cfg: I2cConfig { sda: IMU_SDA_GPIO, scl: IMU_SCL_GPIO, speed: BusSpeed::Standard100k },
};

/// This board as one value; see [`crate::Board`]. A 5×5 panel and an onboard
/// IMU on a private I2C0 bus.
pub const BOARD: crate::Board = crate::Board {
    name: BOARD_NAME,
    imu: Some(crate::I2cAttachment { port: IMU_PORT, addr: IMU_I2C_ADDR }),
    pmic: None,
    touch: None,
    display: None,
    rgb_led: Some(crate::RgbLed { gpio: RGB_LED_GPIO, count: RGB_LED_COUNT, layout: RGB_LED_LAYOUT }),
    selftest: SELFTEST_PADS,
    console: CONSOLE,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_matches_what_the_panel_actually_did() {
        // The four observations the walk produced, asserted against the
        // constant they were turned into. This is the board's claim about its
        // own hardware, so it is tested with the board.
        let p = RGB_LED_LAYOUT.expect("the Matrix has a panel");
        assert_eq!(p.index(4, 4), Some(0), "index 0 is bottom-right");
        assert_eq!(p.index(3, 4), Some(1), "runs leftward along the bottom");
        assert_eq!(p.index(0, 4), Some(4), "bottom-left ends the first row");
        assert_eq!(p.index(4, 3), Some(5), "next row up restarts at the right");
        assert_eq!(p.index(0, 0), Some(24), "top-left is the far end");
    }

    #[test]
    fn the_imu_is_not_on_the_grove_pins() {
        // The IMU sits on a private bus. Wiring a Grove device onto the same
        // controller as the IMU would be a real design decision; sharing the
        // pin numbers by accident would be a bug.
        assert_ne!(IMU_SDA_GPIO, GROVE_SDA_GPIO);
        assert_ne!(IMU_SCL_GPIO, GROVE_SCL_GPIO);
        assert_ne!(IMU_SDA_GPIO, IMU_SCL_GPIO);
    }

    #[test]
    fn the_imu_pins_do_not_collide_with_anything_else_onboard() {
        for pin in [IMU_SDA_GPIO, IMU_SCL_GPIO] {
            assert_ne!(pin, RGB_LED_GPIO, "GPIO{pin} is the LED");
            assert_ne!(pin, BUTTON_GPIO, "GPIO{pin} is the button");
            // 6-11 are the SPI flash the chip executes from.
            assert!(!(6..=11).contains(&pin), "GPIO{pin} is SPI flash");
        }
    }

    #[test]
    fn the_panel_and_the_led_count_agree() {
        // Two constants describing one piece of hardware. If they disagree,
        // an application sizes its frame from one and indexes with the other.
        let p = RGB_LED_LAYOUT.expect("the Matrix has a panel");
        assert_eq!(p.len(), RGB_LED_COUNT);
    }
}
