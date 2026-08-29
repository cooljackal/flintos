---
title: Blinky on a timer
---

[Blinky](/tutorials/blinky/) blinked the LED from a task that **slept** between
each change. That works, but the task spends nearly all its time waiting. This
version hands the job to the kernel's **timer**: `main` arms a repeating callback
and returns, and the timer toggles the LED. Same blink, no busy task.

**No new dependency this time** — the timer lives in `api`, which you already
have, and it's the same `board::user_led()` from Blinky. Flash it with:

```bash
make flash APP=blinky-timer BOARD=board-raspberry-pi-pico
```

**What you should see.** The LED still blinks at 1 Hz and the console still logs
each change — but the log line is now tagged `[isr]`, because it's printed from
the timer callback, which runs in interrupt context, not from a task:

```
[    0][blink] INFO  blinking on a 500 ms timer
[  500][isr] INFO  on
[ 1000][isr] INFO  off
[ 1500][isr] INFO  on
```

## From a delay loop to a timer callback

A timer callback is a plain `fn()`, so it can't hold the `led` handle Blinky kept
in a local. Two things change: the LED moves to a **static** so the callback can
reach it, and Blinky's delay loop becomes a **callback the timer runs**.

```rust ins={5,9-14,17-22,25-41}
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
```

## Three things to know about a timer callback

- **It runs in trap (interrupt) context.** Keep it short and never call
  `sleep_ms` — that would try to suspend whoever it interrupted. A register
  write and a log line are fine.
- **It's a plain `fn()`, so it captures nothing.** Anything it touches lives in a
  `static`: [`Once`] holds the LED handle (set once, read many), an `AtomicBool`
  holds the on/off flag.
- **`main` just arms it and returns.** No task busy-waits; the idle task runs the
  rest of the time and the timer wakes only to flip the pin.

## From Blinky to Blinky on a timer

| | Blinky | Blinky on a timer |
|---|---|---|
| What toggles the LED | a task, in a `sleep` loop | a timer callback |
| Where the LED lives | a local in the task | a `static` (`Once`) |
| While idle | the task sleeps | nothing runs but idle |
| Logged from | the task (`[blink]`) | the callback (`[isr]`) |

## Next: interrupts

A timer callback already *is* an interrupt handler the kernel calls for you. The
next step wires up your own: the `blink` example (`apps/examples/blink`) drives
an addressable RGB LED whose colour is a timed signal, streamed through a
peripheral and refilled from an interrupt you connect yourself.
