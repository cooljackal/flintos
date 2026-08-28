// SPDX-License-Identifier: Apache-2.0

//! Blink the onboard LED — the simplest step past `hello`.
//!
//! Where `hello` logged a word, this drives a pin: one task turns the onboard
//! LED on and off on a half-second delay. It runs on the RP2040 boards, whose
//! LED is a plain GPIO the board opens for us (`board::user_led`) — the
//! Raspberry Pi Pico (GP25) and the Wio RP2040 Mini (GP13):
//!
//!   make flash APP=blinky BOARD=board-raspberry-pi-pico
//!
//! Next: `blink` — an addressable RGB LED, whose timed signal is streamed over a
//! peripheral and refilled from an interrupt.

#![no_std]
#![no_main]

use api::prelude::*;

kernel::flint_app!(main, abi = 2);

fn main() {
    Task::new("blink", blink).spawn().expect("spawn");
}

fn blink() {
    // The board opens the onboard LED's GPIO and hands back a ready handle, so
    // this app never names the Layer-1 GPIO driver itself.
    let led = board::user_led().expect("open the onboard LED");
    loop {
        let _ = led.on();
        log_info!("on");
        sleep_ms(500);
        let _ = led.off();
        log_info!("off");
        sleep_ms(500);
    }
}
