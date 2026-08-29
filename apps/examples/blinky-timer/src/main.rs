// SPDX-License-Identifier: Apache-2.0

//! Blink the onboard LED from a periodic **timer** instead of a delay loop.
//!
//! [`blinky`](../blinky) toggled the LED in a task that slept between each
//! change. Here the kernel's timer does the toggling: `main` opens the LED, arms
//! a repeating 500 ms timer, and returns — no task busy-waits. The callback
//! fires from the timer (trap context), so it stays short and never sleeps.
//!
//! Runs on the RP2040 boards (Raspberry Pi Pico GP25, Wio RP2040 Mini GP13):
//!
//!   make flash APP=blinky-timer BOARD=board-raspberry-pi-pico

#![no_std]
#![no_main]

use api::prelude::*;
use core::sync::atomic::{AtomicBool, Ordering};

kernel::flint_app!(main, abi = 2);

// The callback takes no arguments, so its state lives in `static`s
// both it (an interrupt) and `main` can reach — and the two differ:
// the LED handle is set once and never changes — a fill-once `Once`,
static LED: Once<board::Led> = Once::new();
// while the on/off flag flips every tick — a lock-free `AtomicBool`.
static ON: AtomicBool = AtomicBool::new(false);

fn main() {
    // Open the LED once and hand it to the static.
    LED.init(board::user_led().expect("open the onboard LED"));
    // Arm a repeating timer: `toggle` runs every 500 ms. `main` then
    // returns — the timer drives the blink; nothing loops or sleeps.
    let _ = timer::every_ms(500, toggle);
    log_info!("blinking on a 500 ms timer");
}

// Runs from the timer every 500 ms, in trap context. Keep it
// short and never block — no `sleep_ms` here.
fn toggle() {
    // Read the on/off flag and flip it (`!` means "not").
    // `Relaxed` = we only want the value, nothing else to synchronise.
    let on = !ON.load(Ordering::Relaxed);
    // Store the flipped value back for the next tick.
    ON.store(on, Ordering::Relaxed);
    // Grab the LED from its slot (`None` until `main` fills it)
    if let Some(led) = LED.get() {
        // and drive the pin to match. `let _ =` drops the `Result`; a
        // GPIO write here can't meaningfully fail.
        let _ = led.set(on);
    }
    // Log which state we set — tagged `[isr]`, the timer's context.
    log_info!("{}", if on { "on" } else { "off" });
}
