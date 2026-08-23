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
//! - `esp32_rmt` emits pulse widths. It has never heard of an LED.
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
//! On a board with a panel (Atom Matrix, 25 LEDs): shapes drawn by `(x, y)`
//! through the board's declared layout — a column sweeping right, a row
//! sweeping down, a diagonal — then a walk along the chain logging each index.
//! A correct layout draws straight lines; a wrong one scatters the same lit
//! cells, which is obvious at a glance.
//!
//! Which of the two it does is decided by the board manifest, not by a flag
//! here. `RGB_LED_COUNT` and `RGB_LED_LAYOUT` are facts about the board, and
//! an application that carried them would be carrying someone else's
//! datasheet.
//!
//! Next: `pwm` — LEDC on the same Atom, measuring its own output.

#![no_std]
#![no_main]

use api::task::{self, Task};
use api::CsCell;
use hal::{PinConfig, PinMux, Signal};

use esp32_gpio::{Esp32Gpio, PinMode};
use esp32_rmt::{self as rmt, Entry, Refill, Rmt};
use led_matrix::Layout;
use soc_esp32::addr;
use soc_esp32::dport::{self, ClockBit};
use soc_esp32::pinmux::Esp32PinMux;
use ws2812::{LedStrip, PulseEmitter, Rgb, StripError, Ws2812};

// Only the Atom boards declare an addressable LED. Say which board is missing
// rather than letting the build fail on an unresolved `RGB_LED_GPIO` deep
// inside `led_init`. The board is selected with `--features kernel/board-...`
// now, not an application feature, so this keys on a board fact -- the board's
// own name -- rather than a feature this crate no longer declares. Both Atom
// boards report a name with "ATOM" in it; no other board does.
const _: () = assert!(
    board_name_contains("ATOM"),
    "`blink` drives an onboard addressable LED, and only the M5Stack Atom boards \
     declare one.\n\n\
     \tmake flash APP=blink BOARD=board-m5-atom-matrix   5x5 panel, 25 LEDs\n\
     \tmake flash APP=blink BOARD=board-m5-atom-lite     one LED\n\n\
     To run it on another board, give that board's manifest an `RGB_LED_GPIO`, an \
     `RGB_LED_COUNT` and an `RGB_LED_LAYOUT`."
);

/// Substring test over `board::BOARD_NAME`, usable in a `const` context.
const fn board_name_contains(needle: &str) -> bool {
    let hay = kernel::board::active::BOARD_NAME.as_bytes();
    let ndl = needle.as_bytes();
    if ndl.is_empty() {
        return true;
    }
    if ndl.len() > hay.len() {
        return false;
    }
    let mut i = 0;
    while i + ndl.len() <= hay.len() {
        let mut j = 0;
        while j < ndl.len() && hay[i + j] == ndl[j] {
            j += 1;
        }
        if j == ndl.len() {
            return true;
        }
        i += 1;
    }
    false
}

kernel::flint_app!(main, abi = 2);

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

/// How many LEDs are on the pin, from the manifest.
///
/// This used to be a feature of this application, which meant `blink` carried
/// a fact about someone else's hardware and the two Atoms were told apart by a
/// build flag rather than by the board they are.
const LED_COUNT: usize = kernel::board::active::RGB_LED_COUNT;

/// The panel, if this board has one. `None` on a board with a single LED.
const PANEL: Option<Layout> = kernel::board::active::RGB_LED_LAYOUT;

/// This board's strip: the panel's pixel count, driven through the RMT.
type Strip = Ws2812<RmtEmitter, LED_COUNT>;

/// RMT entries in a full frame: one per bit, 24 bits per LED.
const FRAME_ENTRIES: usize = LED_COUNT * 24;

/// Everything the interrupt shares with the task. `None` until the channel is
/// claimed.
///
/// One cell, not several statics: the interrupt reads the frame while it feeds
/// the channel, and the task writes the frame while it stages the next one, so
/// the two must be excluded from each other as a unit. Every access goes
/// through `with`, which masks interrupts for the closure, so neither side can
/// hold a reference while the other runs — which is the fix for the two live
/// `&mut` to one static this app used to have (#99). Each closure is short:
/// one LED's 24 entries, one half-block refill, or one register read, never a
/// whole frame encode.
///
/// The frame lives in here rather than on the stack for two reasons: 25 LEDs
/// is 2400 bytes against a 4 KiB task stack the trap handler also runs on, and
/// the interrupt reads it long after the call that started the stream has
/// returned.
static LINK: CsCell<Option<Link>> = CsCell::new(None);

/// The channel, its refill cursor, and the frame it is being fed.
struct Link {
    rmt: Rmt,
    refill: Refill,
    /// Set by the interrupt when there is nothing left to feed; cleared by the
    /// task when it starts a frame.
    done: bool,
    frame: [Entry; FRAME_ENTRIES],
}

/// Bridges `ws2812` to this board's RMT channel.
///
/// Owned by the strip and touched only by the task. What the interrupt needs
/// lives in `LINK`; this holds the task's own bookkeeping — how far into the
/// frame staging has reached, and whether the "it streamed" line has printed.
struct RmtEmitter {
    /// How many entries of the current frame have been staged into `LINK`.
    staged: usize,
    /// Whether the "it streamed" line has been printed. Once is enough; a
    /// stall reports every time.
    reported: bool,
}

impl RmtEmitter {
    const fn new() -> Self {
        Self {
            staged: 0,
            reported: false,
        }
    }
}

impl PulseEmitter for RmtEmitter {
    fn ns_per_tick(&self) -> u32 {
        NS_PER_TICK
    }

    fn begin(&mut self) -> Result<(), StripError> {
        self.staged = 0;
        Ok(())
    }

    fn emit(&mut self, pulses: &[(u16, u16)]) -> Result<(), StripError> {
        // Refusing beats overwriting: a truncated frame lights some pixels and
        // leaves the rest as they were, which reads as a broken wire.
        if self.staged + pulses.len() > FRAME_ENTRIES {
            return Err(StripError::OutOfRange);
        }
        // One LED's worth under the mask, not the whole frame: `ws2812` calls
        // this once per pixel with 24 pairs.
        LINK.with(|link| {
            let link = link.as_mut().ok_or(StripError::Transport)?;
            for (high, low) in pulses {
                // Both pulses of a bit in one entry, so a bit is never split
                // across a refill boundary and cannot be half-sent.
                link.frame[self.staged] = Entry::new(true, *high, false, *low);
                self.staged += 1;
            }
            Ok(())
        })
    }

    fn finish(&mut self) -> Result<(), StripError> {
        self.send_staged_frame().map_err(|()| StripError::Transport)
    }
}

impl RmtEmitter {
    /// Stream whatever `emit` staged into `LINK`, and wait for it to finish.
    ///
    /// Unchanged in sequence from the version verified on hardware -- only the
    /// way it reaches the channel moved.
    fn send_staged_frame(&mut self) -> Result<(), ()> {
        LINK.with(|link| {
            let link = link.as_mut().ok_or(())?;
            link.done = false;
            // SAFETY: the channel's registers are this program's alone (see
            // `led_init`), and `frame` is kept alive and unchanged until the
            // stream finishes: it lives in `LINK`, its only writer is `emit`,
            // and the strip does not call `emit` again until `finish` returns.
            // `rmt_isr` is connected and services every threshold.
            link.refill = unsafe { link.rmt.start_stream(&link.frame) };
            Ok(())
        })?;

        // Completion is TX_END, which the channel sets when it stops. The
        // interrupt's own "nothing left to feed" is a weaker statement: it needs
        // one more threshold after the terminator is written, and the channel
        // stops at the terminator, so that threshold often never comes.
        let mut waited = 0;
        let mut finished = false;
        for _ in 0..10 {
            task::sleep_ms(1);
            waited += 1;
            // SAFETY: reads one interrupt status register of a channel this
            // program owns.
            if LINK.with(|l| l.as_ref().is_some_and(|l| unsafe { l.rmt.stream_done() })) {
                finished = true;
                break;
            }
        }

        // Whether the interrupt actually fed the whole frame. Without it the
        // channel emits its first 64 entries and stops, and on a panel that is
        // two lit LEDs and 23 dark ones -- which looks like a wiring fault, not
        // a missed refill.
        let fed = LINK.with(|l| l.as_ref().map_or(0, |l| l.refill.written()));
        if !finished || fed != FRAME_ENTRIES {
            api::log_error!(
                "stream stalled: {} of {} entries fed, TX_END {} after {} ms",
                fed,
                FRAME_ENTRIES,
                if finished { "set" } else { "NOT set" },
                waited
            );
            return Err(());
        }
        if !self.reported {
            self.reported = true;
            api::log_info!(
                "streamed {} entries via {} refills in {} ms",
                fed,
                fed.div_ceil(rmt::HALF_BLOCK),
                waited
            );
        }

        // Latch. The datasheet wants the line idle at least 50 us; a tick is
        // far more, and costs nothing here.
        task::sleep_ms(1);
        Ok(())
    }
}

fn main() {
    if Task::new("blink", blink).spawn().is_none() {
        api::log_error!("could not start the blink task");
    }
}

fn blink() {
    if led_init().is_none() {
        // Nothing to do without a channel; say so and stop, rather than leave a
        // board that looks alive and does nothing.
        api::log_error!("could not claim RMT channel {} on GPIO {}", LED_CHANNEL, LED_PIN);
        task::exit();
    }

    api::log_info!(
        "{} LED(s) on GPIO {} via RMT channel {}, {} entries per frame",
        LED_COUNT,
        LED_PIN,
        LED_CHANNEL,
        FRAME_ENTRIES
    );

    let mut strip: Strip = Ws2812::new(RmtEmitter::new());
    match PANEL {
        Some(panel) => panel_demo(&mut strip, panel),
        None => colour_cycle(&mut strip),
    }
}

/// One LED: red, green, blue, off.
fn colour_cycle(strip: &mut Strip) -> ! {
    let sequence = [
        ("red", Rgb::RED.dim(10)),
        ("green", Rgb::GREEN.dim(10)),
        ("blue", Rgb::BLUE.dim(10)),
        ("off", Rgb::OFF),
    ];
    loop {
        for (name, colour) in sequence {
            let _ = strip.clear();
            let _ = strip.set(0, colour);
            show(strip);
            api::log_info!("{}", name);
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
fn panel_demo(strip: &mut Strip, panel: Layout) -> ! {
    loop {
        // Shapes first: the layout is measured now, so the interesting
        // question is whether it is right, not what it is.
        draw_by_coordinates(strip, panel);
        for i in 0..LED_COUNT {
            let _ = strip.clear();
            // Dim: at full scale a panel this close is genuinely painful, and
            // a washed-out photo is harder to read the position off.
            let _ = strip.set(i, Rgb::new(0, 0, 255).dim(8));
            show(strip);
            api::log_info!("index {}", i);
            task::sleep_ms(600);
        }
    }
}

/// Sweep a column, then a row, then a diagonal, addressing cells by `(x, y)`.
///
/// This checks the layout rather than the chain. A correct mapping draws a
/// straight line moving in the direction named in the log; a wrong one
/// scatters the same number of lit cells across the panel, which is obvious at
/// a glance and impossible to talk yourself out of.
fn draw_by_coordinates(strip: &mut Strip, panel: Layout) {
    let (w, h) = (panel.width, panel.height);

    for x in 0..w {
        api::log_info!("column x={} (expect a vertical line, moving right)", x);
        paint(strip, panel, |cx, cy| cx == x && cy < h);
        task::sleep_ms(500);
    }
    for y in 0..h {
        api::log_info!("row y={} (expect a horizontal line, moving down)", y);
        paint(strip, panel, |cx, cy| cy == y && cx < w);
        task::sleep_ms(500);
    }
    api::log_info!("diagonal (expect top-left to bottom-right)");
    paint(strip, panel, |cx, cy| cx == cy);
    task::sleep_ms(1500);
}

/// Light every cell the predicate accepts, addressing them through the layout.
fn paint(strip: &mut Strip, panel: Layout, lit: impl Fn(usize, usize) -> bool) {
    let _ = strip.clear();
    for y in 0..panel.height {
        for x in 0..panel.width {
            if !lit(x, y) {
                continue;
            }
            // `index` refuses a cell off the panel rather than wrapping, so a
            // bad coordinate cannot quietly light the wrong LED.
            match panel.index(x, y) {
                Some(i) => {
                    let _ = strip.set(i, Rgb::new(0, 255, 0).dim(8));
                }
                None => api::log_error!("({}, {}) is off the panel", x, y),
            }
        }
    }
    show(strip);
}

/// Push a frame, logging rather than swallowing a failure.
fn show(strip: &mut Strip) {
    if let Err(e) = strip.show() {
        api::log_error!("show failed: {:?}", e);
    }
}

/// Called from the trap handler when the channel wants its next half block.
///
/// Deliberately tiny. The deadline is about 40 µs and it runs with the
/// scheduler's own interrupt masked; the critical section `with` takes is the
/// same mask the trap handler already holds, so it costs a register write and
/// nothing else.
fn rmt_isr() {
    LINK.with(|link| {
        if let Some(link) = link {
            if !link.done {
                // SAFETY: this is the channel's threshold interrupt, `frame` is
                // the sequence `start_stream` was given, and nothing else can
                // be inside `LINK` while this closure runs.
                link.done = unsafe { link.rmt.service(&link.frame, &mut link.refill) };
            }
        }
    });
}

/// Bring up the clock, the pad, the channel and the interrupt.
///
/// Claims RMT channel 0, GPIO `LED_PIN` and CPU interrupt `LED_CPU_INT` for the
/// life of the program. `None` means one of them was refused, and the log says
/// which.
fn led_init() -> Option<()> {
    // The peripheral answers reads with plausible garbage while its clock is
    // gated, so this has to come before anything else touches its registers.
    // SAFETY: the RMT is this program's to gate -- nothing else in the build
    // claims it -- and `enable` is itself safe against the other core and
    // interrupts.
    unsafe { dport::enable(ClockBit::RMT) };

    // Output enable for the pad. The matrix leaves output enable with the
    // peripheral, but esp-idf sets the direction here too and this is not the
    // place to find out whether that is load-bearing -- an unenabled pad is a
    // dark LED with nothing to say about why.
    Esp32Gpio::instance().set_mode(LED_PIN, PinMode::Output).ok()?;

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
    // SAFETY: nothing else in this build claims `LED_CHANNEL`, and its output
    // signal was routed to `LED_PIN` just above.
    let rmt = unsafe { Rmt::new(LED_CHANNEL, divider)? };

    // Point the peripheral at a CPU interrupt. Without this the RMT's own
    // interrupt enables set happily and nothing is ever delivered -- there was
    // no crossbar routing in this kernel at all before streaming needed one.
    //
    // SAFETY: `rmt_isr` runs as one masked closure that acknowledges the
    // threshold (inside `Rmt::service`) and copies at most half a block; it
    // never blocks.
    if let Err(e) = unsafe { kernel::interrupt::connect_at(addr::IRQ_RMT, LED_CPU_INT, rmt_isr) } {
        api::log_error!("cannot connect RMT to CPU interrupt {}: {:?}", LED_CPU_INT, e);
        return None;
    }

    LINK.with(|link| {
        *link = Some(Link {
            rmt,
            refill: Refill::new(0),
            done: true,
            frame: [Entry::END; FRAME_ENTRIES],
        });
    });
    Some(())
}
