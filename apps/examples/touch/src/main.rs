// SPDX-License-Identifier: Apache-2.0

//! Reports the M5Stack Core2's touch coordinates from the FT6336U.
//!
//! The touch controller shares the internal I2C0 bus with the IMU and the
//! AXP192 PMIC — `board::touch_bus()` hands back the same controller the board
//! opened at boot, so this app opens no peripheral itself.
//!
//! ```text
//!   Layer 3   ft6336u     the touch part number
//!   Layer 2   i2c-bus     addressing and framing
//!   Layer 1   esp32-i2c   the controller's registers (opened by the board)
//! ```
//!
//! It polls at ~30 Hz and logs each touch as it moves. The panel's y runs past
//! the 240-pixel screen height because the three capacitive buttons below the
//! display share the touch surface.

#![no_std]
#![no_main]

use api::task;
use hal::types::Priority;

use board::touch_bus;
use ft6336u::Ft6336u;
use kernel::board::active as manifest;

// The board is selected with `--features kernel/board-...`, so this guard keys
// on the fact the app needs -- that the board declares a touch panel -- not a
// board name. Only the Core2 sets `BOARD.touch` today.
const _: () = assert!(
    manifest::BOARD.touch.is_some(),
    "`touch` reads a capacitive touch panel, and only the M5Stack Core2 declares \
     one.\n\n\
     \tmake flash APP=touch BOARD=board-m5-core2\n\n\
     To run this elsewhere, give that board's manifest a \
     `touch: Some(I2cAttachment {{ .. }})` in its `BOARD`."
);

kernel::flint_app!(main, abi = 2);

fn main() {
    task::spawn("touch", touch, Priority::Normal(1), 4096);
}

fn touch() {
    let ctrl = match touch_bus() {
        Ok(c) => c,
        Err(e) => {
            api::log_error!("no touch bus: {}", e);
            return;
        }
    };
    let addr = manifest::BOARD.touch.expect("guarded above").addr;
    let device = ctrl.device(addr);
    let panel = Ft6336u::new(&device);

    match (panel.is_present(), panel.chip_id(), panel.focaltech_id()) {
        (Ok(true), Ok(chip), Ok(vendor)) => {
            api::log_info!("FT6336U present: chip_id=0x{:02X} vendor=0x{:02X}", chip, vendor)
        }
        (Ok(false), _, Ok(vendor)) => {
            api::log_error!("no FT6336U at 0x{:02X}: vendor byte 0x{:02X}", addr, vendor)
        }
        _ => api::log_error!("could not read the touch controller at 0x{:02X}", addr),
    }

    api::log_info!("touch the screen -- coordinates follow");
    let mut was_down = false;
    loop {
        match panel.touch1() {
            Ok(Some(t)) => {
                api::log_info!("touch ({:>3}, {:>3}) id={} {:?}", t.x, t.y, t.id, t.event);
                was_down = true;
            }
            Ok(None) => {
                if was_down {
                    api::log_info!("release");
                    was_down = false;
                }
            }
            Err(e) => api::log_error!("touch read failed: {:?}", e),
        }
        task::sleep_ms(33);
    }
}
