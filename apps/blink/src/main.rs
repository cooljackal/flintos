// SPDX-License-Identifier: Apache-2.0

//! Drives the M5Stack Atom's onboard addressable LED, or its 5×5 panel.
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
//! # Two modes
//!
//! Default (Atom Lite, one LED): red, green, blue, off, one second each. Wrong
//! *colour* in the right order means the byte order is wrong — GRB, not RGB.
//! Flicker or the wrong colour at random means pulse widths outside the
//! ±150 ns window. Nothing at all means the signal never reached the pad.
//!
//! With `--features atom-matrix` (Atom Matrix, 25 LEDs): one LED lit at a
//! time, walking the chain from index 0, logging each index as it goes. That
//! is a 600-entry frame against a 64-entry block, so it exercises the
//! streaming path — and it is how the panel's physical layout gets *measured*
//! rather than guessed, which is what #52 needs before it can name a layout.

#![no_std]
#![no_main]

use core::ptr::{addr_of, addr_of_mut};

use api::task;
use hal::types::Priority;
use hal::{PinConfig, PinMux, Signal};

use esp32_gpio::{Esp32Gpio, PinMode};
use soc_esp32::dport::{self, ClockBit};
use soc_esp32::pinmux::Esp32PinMux;
use soc_esp32::rmt::{self, Entry, Refill, Rmt};
use soc_esp32::{addr, intr_map};
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

/// CPU interrupt for the RMT source.
///
/// 13 is external, level 1 and otherwise unused. `intr_map::route` rejects
/// anything the kernel could not service, so a bad choice here is a build-time
/// `Result`, not a board that dies at its first interrupt.
const LED_CPU_INT: u8 = 13;

/// 125 ns per tick. The WS2812's pulse widths are near-multiples of it, and it
/// divides the 80 MHz APB exactly (divider 10), so no timing error accumulates
/// from the clock itself.
const NS_PER_TICK: u32 = 125;

/// How many LEDs are on the pin.
///
/// A feature rather than a manifest constant because `RGB_LED_GPIO` is all the
/// Atom manifest says, and a Lite and a Matrix are the same pin on the same
/// board name — see #53.
#[cfg(feature = "atom-matrix")]
const LED_COUNT: usize = 25;
#[cfg(not(feature = "atom-matrix"))]
const LED_COUNT: usize = 1;

/// RMT entries in a full frame: one per bit, 24 bits per LED.
const FRAME_ENTRIES: usize = LED_COUNT * 24;

/// The frame being transmitted.
///
/// Static rather than on the stack: 25 LEDs is 2400 bytes and the task stack is
/// 4 KiB, which the trap handler also runs on. It has to outlive the call that
/// starts the stream in any case — the interrupt reads it long afterwards.
static mut FRAME: [Entry; FRAME_ENTRIES] = [Entry::END; FRAME_ENTRIES];

/// Everything the interrupt needs. `None` until the channel is claimed.
static mut STREAM: Option<Stream> = None;

struct Stream {
    rmt: Rmt,
    refill: Refill,
    done: bool,
}

fn main() {
    task::spawn("blink", blink, Priority::Normal(1), 4096);
}

fn blink() {
    if unsafe { led_init() }.is_none() {
        // Returning would leave a board that looks alive and does nothing.
        api::log_error!("[blink] could not claim RMT channel {} on GPIO {}", LED_CHANNEL, LED_PIN);
        loop {
            task::sleep_ms(1000);
        }
    }

    api::log_info!(
        "[blink] {} LED(s) on GPIO {} via RMT channel {}, {} entries per frame",
        LED_COUNT,
        LED_PIN,
        LED_CHANNEL,
        FRAME_ENTRIES
    );

    if LED_COUNT == 1 {
        colour_cycle()
    } else {
        walk_the_chain()
    }
}

/// One LED: red, green, blue, off.
fn colour_cycle() -> ! {
    let sequence = [
        ("red", Rgb::RED.dim(10)),
        ("green", Rgb::GREEN.dim(10)),
        ("blue", Rgb::BLUE.dim(10)),
        ("off", Rgb::OFF),
    ];
    loop {
        for (name, colour) in sequence {
            let mut frame = [Rgb::OFF; LED_COUNT];
            frame[0] = colour;
            show(&frame);
            api::log_info!("[blink] {}", name);
            task::sleep_ms(1000);
        }
    }
}

/// A panel: one LED at a time along the chain, index logged as it goes.
///
/// This is a measuring instrument, not a demo. Watching which physical cell
/// lights for each index is what establishes the panel's layout — whether it
/// runs in rows or columns, from which corner, and whether alternate lines
/// reverse. Guessing any of that is how #52 would ship a driver that lights
/// the wrong pixel.
fn walk_the_chain() -> ! {
    loop {
        for i in 0..LED_COUNT {
            let mut frame = [Rgb::OFF; LED_COUNT];
            // Dim: at full scale a panel this close is genuinely painful, and
            // a washed-out photo is harder to read the position off.
            frame[i] = Rgb::new(0, 0, 255).dim(8);
            show(&frame);
            api::log_info!("[blink] index {}", i);
            task::sleep_ms(600);
        }
    }
}

/// Encode `colours` and stream them, waiting for the transmission to finish.
fn show(colours: &[Rgb; LED_COUNT]) {
    let mut bits = [(0u16, 0u16); FRAME_ENTRIES];
    if ws2812::encode(colours, Timing::WS2812, NS_PER_TICK, &mut bits).is_none() {
        api::log_error!("[blink] encode refused a {}-entry buffer", FRAME_ENTRIES);
        return;
    }

    // One RMT entry per bit: high for the value's high time, then low. Both
    // pulses are in the same entry, so a bit is never split across a refill
    // boundary and cannot be half-sent.
    unsafe {
        let frame = &mut *addr_of_mut!(FRAME);
        for (entry, (high, low)) in frame.iter_mut().zip(bits) {
            *entry = Entry::new(true, high, false, low);
        }
    }

    unsafe {
        let Some(stream) = &mut *addr_of_mut!(STREAM) else {
            return;
        };
        stream.done = false;
        stream.refill = stream.rmt.start_stream(&*addr_of!(FRAME));
    }

    // The frame is ~1.25 µs per bit, so 600 bits is about 750 µs. Sleeping
    // rather than spinning is the point of refilling from an interrupt: the
    // scheduler runs everything else while the panel clocks out.
    for _ in 0..10 {
        task::sleep_ms(1);
        if unsafe { (*addr_of!(STREAM)).as_ref().is_some_and(|s| s.done) } {
            break;
        }
    }
    // Latch. The datasheet wants the line idle at least 50 µs; a tick is far
    // more, and costs nothing here.
    task::sleep_ms(1);
}

/// Called from the trap handler when the channel wants its next half block.
///
/// Deliberately tiny. The deadline is about 40 µs and it runs with the
/// scheduler's own interrupt masked.
fn rmt_isr() {
    unsafe {
        if let Some(stream) = &mut *addr_of_mut!(STREAM) {
            if !stream.done {
                stream.done = stream.rmt.service(&*addr_of!(FRAME), &mut stream.refill);
            }
        }
    }
}

/// Bring up the clock, the pad, the channel and the interrupt.
///
/// # Safety
/// Claims RMT channel 0, GPIO `LED_PIN` and CPU interrupt `LED_CPU_INT` for
/// the life of the program.
unsafe fn led_init() -> Option<()> {
    // The peripheral answers reads with plausible garbage while its clock is
    // gated, so this has to come before anything else touches its registers.
    dport::enable(ClockBit::RMT);

    // Output enable for the pad. The matrix leaves output enable with the
    // peripheral, but esp-idf sets the direction here too and this is not the
    // place to find out whether that is load-bearing -- an unenabled pad is a
    // dark LED with nothing to say about why.
    let gpio = Esp32Gpio::new(addr::GPIO_BASE);
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
    let rmt = Rmt::new(LED_CHANNEL, divider)?;

    // Point the peripheral at a CPU interrupt. Without this the RMT's own
    // interrupt enables set happily and nothing is ever delivered -- there was
    // no crossbar routing in this kernel at all before streaming needed one.
    if let Err(e) = intr_map::route(addr::IRQ_RMT, LED_CPU_INT) {
        api::log_error!("[blink] cannot route RMT to CPU interrupt {}: {:?}", LED_CPU_INT, e);
        return None;
    }
    if !kernel::interrupt::register(LED_CPU_INT, rmt_isr) {
        api::log_error!("[blink] CPU interrupt {} already has a handler", LED_CPU_INT);
        return None;
    }
    kernel::arch::registers::enable_interrupt(LED_CPU_INT as u32);

    STREAM = Some(Stream {
        rmt,
        refill: Refill::new(0),
        done: true,
    });
    Some(())
}
