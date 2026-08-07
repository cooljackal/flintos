// SPDX-License-Identifier: Apache-2.0

//! Drives the M5Stack Atom's onboard SK6812 LED.
//!
//! The Atom's board manifest has declared `RGB_LED_GPIO = 27` since the board
//! was added, with nothing able to drive it. This is what makes that constant
//! mean something, and it is the on-hardware test for the RMT register map:
//! the encoder is host-tested, but no host test can tell you whether
//! `RMT_CH0CONF1_REG` is where you think it is. A lit LED can.
//!
//! # What it demonstrates
//!
//! Three layers, composed here and nowhere else:
//!
//! - `ws2812` turns a colour into pulse widths. It depends only on `api` and
//!   has never heard of an ESP32.
//! - `soc_esp32::rmt` emits pulse widths. It has never heard of an LED.
//! - this file knows both exist, and which pin the board put between them.
//!
//! The kernel is not in that list, which is the intended shape: it depends on
//! no driver but the console UART, so an application wanting a peripheral
//! names it in its own `Cargo.toml`.
//!
//! # Reading the result
//!
//! Red, green, blue, off, one second each. Wrong *colour* in the right order
//! means the byte order is wrong — GRB, not RGB. Flicker or the wrong colour
//! at random means pulse widths outside the ±150 ns window. Nothing at all
//! means the signal never reached the pad.

#![no_std]
#![no_main]

use api::task;
use hal::types::Priority;
use hal::{PinConfig, PinMux, Signal};

use esp32_gpio::{Esp32Gpio, PinMode};
use soc_esp32::dport::{self, ClockBit};
use soc_esp32::pinmux::Esp32PinMux;
use soc_esp32::rmt::{self, Entry, Rmt};
use ws2812::{Rgb, Timing};

// Only the Atom declares an addressable LED, so only the Atom can run this.
// Say which board is missing rather than letting the build fail on an
// unresolved `RGB_LED_GPIO` deep inside `led_init`.
#[cfg(not(feature = "board-m5-atom"))]
compile_error!(
    "`blink` drives an onboard addressable LED, and the M5Stack Atom is the only \
     board whose manifest declares one.\n\n    make flash APP=blink BOARD=board-m5-atom\n\n\
     To run it on another board, add an `RGB_LED_GPIO` to that board's manifest \
     and a feature here."
);

kernel::flint_app!(main, abi = 1);

/// The pin the board put the LED on. From the manifest, not from a datasheet
/// read at coding time — that is what the manifest is for.
const LED_PIN: u8 = kernel::board::active::RGB_LED_GPIO;

/// RMT channel 0. Nothing else in this build claims one.
const LED_CHANNEL: u8 = 0;

/// 125 ns per tick. The WS2812's pulse widths are near-multiples of it, and it
/// divides the 80 MHz APB exactly (divider 10), so no timing error accumulates
/// from the clock itself.
const NS_PER_TICK: u32 = 125;

/// Bits in one LED's frame.
const BITS: usize = 24;

/// Time to leave the line idle so the LED latches the frame. The datasheet
/// says at least 50 µs; `Timing::WS2812` uses 80 µs for margin.
const LATCH_US: u32 = Timing::WS2812.reset_us;

fn main() {
    task::spawn("blink", blink, Priority::Normal(1), 4096);
}

fn blink() {
    let Some(mut led) = (unsafe { led_init() }) else {
        // Returning would leave a task that spins doing nothing while the
        // board looks alive. Say what failed and stop.
        api::log_error!("[blink] could not claim RMT channel {} on GPIO {}", LED_CHANNEL, LED_PIN);
        loop {
            task::sleep_ms(1000);
        }
    };

    api::log_info!(
        "[blink] SK6812 on GPIO {} via RMT channel {}, {} ns/tick",
        LED_PIN,
        LED_CHANNEL,
        NS_PER_TICK
    );

    // Dimmed hard on purpose. These LEDs are unpleasant at full scale and this
    // one is a few centimetres from whoever is holding the board.
    let sequence = [
        ("red", Rgb::RED.dim(10)),
        ("green", Rgb::GREEN.dim(10)),
        ("blue", Rgb::BLUE.dim(10)),
        ("off", Rgb::OFF),
    ];

    loop {
        for (name, colour) in sequence {
            match show(&mut led, colour) {
                true => api::log_info!("[blink] {}", name),
                // A refused frame is a programming error, not a hardware
                // fault, and silently skipping it would look like a dead LED.
                false => api::log_error!("[blink] frame refused for {}", name),
            }
            task::sleep_ms(1000);
        }
    }
}

/// Bring up the clock, the pad and the channel.
///
/// # Safety
/// Claims RMT channel 0 and GPIO `LED_PIN` for the life of the program.
unsafe fn led_init() -> Option<Rmt> {
    // The peripheral answers reads with plausible garbage while its clock is
    // gated, so this has to come before anything else touches its registers.
    dport::enable(ClockBit::RMT);

    // Output enable for the pad. The matrix leaves output enable with the
    // peripheral, but esp-idf sets the direction here too and this is not the
    // place to find out whether that is load-bearing -- an unenabled pad is a
    // dark LED with nothing to say about why.
    let gpio = Esp32Gpio::new(soc_esp32::addr::GPIO_BASE);
    gpio.set_mode(LED_PIN, PinMode::Output).ok()?;

    // Push-pull: one driver, one LED, no bus to share. `route` handles IO_MUX
    // function select and the matrix entry, and refuses a pin that cannot
    // drive an output.
    Esp32PinMux::new()
        .route(Signal::RmtOut(LED_CHANNEL), LED_PIN, PinConfig::PUSH_PULL)
        .ok()?;

    let (divider, actual_ns) = rmt::divider_for_ns(NS_PER_TICK);
    // If the divider could not deliver the tick asked for, every pulse below is
    // scaled by the difference and the LED shows something arbitrary.
    debug_assert_eq!(actual_ns, NS_PER_TICK);

    Rmt::new(LED_CHANNEL, divider)
}

/// Encode one colour and send it. Returns false if the frame was refused.
fn show(led: &mut Rmt, colour: Rgb) -> bool {
    let mut bits = [(0u16, 0u16); BITS];
    if ws2812::encode(&[colour], Timing::WS2812, NS_PER_TICK, &mut bits).is_none() {
        return false;
    }

    // One RMT entry per bit: high for the value's high time, then low. Both
    // pulses are in the same entry, so a bit is never split across a FIFO
    // write and cannot be half-sent.
    let mut entries = [Entry::END; BITS];
    let mut ticks = 0u32;
    for (entry, (high, low)) in entries.iter_mut().zip(bits) {
        *entry = Entry::new(true, high, false, low);
        ticks += high as u32 + low as u32;
    }

    if !unsafe { led.transmit(&entries) } {
        return false;
    }

    // `transmit` returns as soon as the channel is started, so the wait is
    // ours. It is about 30 µs of frame plus the latch -- during which the
    // scheduler runs everything else, which is the entire reason the SoC layer
    // does not busy-wait on our behalf.
    let frame_us = rmt::frame_ns(ticks, NS_PER_TICK).div_ceil(1000);
    task::sleep_ms((frame_us + LATCH_US).div_ceil(1000).max(1));
    true
}
