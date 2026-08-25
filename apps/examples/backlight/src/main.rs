// SPDX-License-Identifier: Apache-2.0

//! Switches the M5Stack Core2's LCD backlight on and off through the AXP192
//! PMIC — the visible proof that a board's power rail can be driven at runtime.
//!
//! The board already brought the rails up at boot (`board::power_init`, which
//! set LDO2 = 3.3 V for the peripheral logic and DCDC3 = 2.8 V for the
//! backlight, in that order). So the screen is lit before this app runs; the
//! app then toggles DCDC3 once a second, and the backlight visibly blinks.
//!
//! ```text
//!   Layer 3   axp192      the PMIC part number
//!   Layer 2   i2c-bus     addressing and framing
//!   Layer 1   esp32-i2c   the controller's registers (opened by the board)
//! ```
//!
//! Each cycle also logs the rail's reported state and, if a battery is
//! attached, its voltage and charge status — a second thing the AXP192 is for.

#![no_std]
#![no_main]

use api::task;
use api::Priority;

use axp192::{Axp192, Rail};
use board::pmic_bus;
use kernel::board::active as manifest;

// The board is selected with `--features kernel/board-...`, so this guard keys
// on the fact the app needs -- that the board declares a PMIC -- not a board
// name. Only the Core2 sets `BOARD.pmic` today; this fires on any other board.
const _: () = assert!(
    manifest::BOARD.pmic.is_some(),
    "`backlight` switches a power rail through an onboard PMIC, and only the \
     M5Stack Core2 declares one.\n\n\
     \tmake flash APP=backlight BOARD=board-m5-core2\n\n\
     To run this elsewhere, give that board's manifest a \
     `pmic: Some(PmicAttachment {{ .. }})` in its `BOARD`."
);

kernel::flint_app!(main, abi = 2);

fn main() {
    task::spawn("backlight", backlight, Priority::Normal(1), 4096);
}

fn backlight() {
    // The board opened this controller and applied the boot rail list already;
    // `pmic_bus()` hands back the same `&'static` controller.
    let ctrl = match pmic_bus() {
        Ok(c) => c,
        Err(e) => {
            api::log_error!("no PMIC bus: {}", e);
            return;
        }
    };
    let addr = manifest::BOARD.pmic.expect("guarded above").addr;
    let device = ctrl.device(addr);
    let axp = Axp192::new(&device);

    // One reading of the battery side, once, so a run says whether a pack is
    // attached without spamming every cycle.
    match (axp.battery_present(), axp.battery_millivolts(), axp.charging()) {
        (Ok(true), Ok(mv), Ok(chg)) => {
            api::log_info!("battery: {} mV, {}", mv, if chg { "charging" } else { "discharging" })
        }
        (Ok(false), _, _) => api::log_info!("battery: none attached"),
        _ => api::log_warn!("battery: could not read status"),
    }

    let mut on = false;
    loop {
        on = !on;
        match axp.set_rail_enabled(Rail::Dcdc3, on) {
            Ok(()) => api::log_info!("backlight {}", if on { "on" } else { "off" }),
            Err(e) => api::log_error!("backlight toggle failed: {:?}", e),
        }
        task::sleep_ms(1000);
    }
}
