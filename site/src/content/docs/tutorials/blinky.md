---
title: Blinky
---

[Hello World](/tutorials/hello-world/) logged a word. **Blinky** keeps the same
one-task skeleton and points it at real hardware: it turns the board's onboard
LED on and off on a delay. It's the step from "the board boots" to "my code
moved a pin."

This one runs on the **RP2040 boards**, whose onboard LED is a plain GPIO — the
Raspberry Pi Pico (GP25) and the Wio RP2040 Mini (GP13). Flash it with:

```bash
make flash APP=blinky BOARD=board-raspberry-pi-pico
```

**What you should see.** The LED blinks at 1 Hz — on for half a second, off for
half a second — and the console echoes each change:

```
[  500][blink] INFO  on
[ 1000][blink] INFO  off
[ 1500][blink] INFO  on
[ 2000][blink] INFO  off
```

If the console prints `on`/`off` but the LED stays dark, the pin toggled but the
LED isn't on it — check the board's `USER_LED` pin.

## 1. Add the library it needs

Hello World depended on `api` and `kernel` only. Blinky also needs the **board**,
because the board is what opens the LED's GPIO and hands the app a ready handle —
an app never touches the Layer-1 GPIO driver itself. Scaffold from `hello` and
add the dependency:

```bash
make new-app NAME=blinky                          # copy the hello template
cargo add board --path ../../../board -p blinky   # add the board library
```

`board` isn't a driver, so it's a plain `cargo add`. When an app needs an actual
**driver** — as the next tutorial does for an addressable LED — the make helpers
add and drop them for you:

```bash
make enable-driver APP=<app> DRIVER=<name>    # add a driver dependency
make disable-driver APP=<app> DRIVER=<name>   # remove it again
```

### `Cargo.toml` — the one new dependency

```toml ins={5}
[dependencies]
kernel = { path = "../../../kernel", default-features = false }
api = { path = "../../../api" }
# New relative to Hello World — the board opens the LED for us:
board = { path = "../../../board" }
```

## 2. The task drives a pin

The skeleton is unchanged from Hello World — `main` spawns one task and returns.
Only the task body is new: instead of logging a counter, it opens the LED and
toggles it.

```rust ins={20-33}
#![no_std]
#![no_main]

use api::prelude::*;

kernel::flint_app!(main, abi = 2);

fn main() {
    // Same shape as Hello World — spawn one task and return.
    Task::new("blink", blink).spawn().expect("spawn");
}

fn blink() {
    // The board opens the onboard LED's GPIO and hands back a ready handle, so
    // this app never names the Layer-1 GPIO driver itself.
    let led = board::user_led().expect("open the onboard LED");
    loop {
        // On, wait, off, wait — forever.
        let _ = led.on();
        log_info!("on");
        sleep_ms(500);
        let _ = led.off();
        log_info!("off");
        sleep_ms(500);
    }
}
```

`board::user_led()` returns a small `Led` handle with `on()`, `off()` and
`set(bool)`. Under it, the board opened the GPIO the manifest calls `USER_LED`
and set it to output — bring-up the app doesn't have to know about. `let _ =`
ignores the `Result`: a GPIO write can't really fail here, and a blink loop that
returned early on one is worse than one that keeps going.

## From Hello World to Blinky

| | Hello World | Blinky |
|---|---|---|
| Dependencies | `api`, `kernel` | **+ `board`** |
| The task does | logs a counter | toggles the onboard LED |
| You see | text on the console | a blinking LED |

## Next: drive it from a timer

This blink is a busy `sleep`/`toggle` loop — the task spends nearly all its time
waiting. Next, [Blinky on a timer](/tutorials/blinky-timer/) hands the toggling
to the kernel's timer, so `main` just arms a callback and returns — no busy task.
Then, later, an **addressable** RGB LED whose timed signal is refilled from an
interrupt you connect yourself.
