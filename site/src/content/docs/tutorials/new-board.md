---
title: Add a board
---

[Blinky](/tutorials/blinky/) blinked the Pico's **onboard** LED on GP25. But
that pin is a board fact, not an app fact — solder your LED to a different pin,
or use a carrier that wires it elsewhere, and GP25 is wrong. The app shouldn't
have to care. A **board manifest** is where FlintOS keeps that fact: the app
asks for "the user LED" and the manifest says which pin.

This tutorial makes a new board by **cloning the Pico** and changing that one
fact — the user LED becomes an external LED on **GP16** — then flashes the same
Blinky at it, unchanged.

**What you'll need.** A Pico, and an LED wired to **GP16**: the long leg
(anode) through a ~330 Ω resistor to GP16, the short leg to any GND pin.

## 1. Scaffold the board from a template

A board is a file of facts under `board/src/`, plus a little wiring that makes it
selectable. `make new-board` does all of that for you — it clones an existing
board and registers the copy:

```bash
make new-board NAME=pico-ext-led FROM=board-raspberry-pi-pico
```

`FROM` is the template to copy (the Pico here; it defaults to the Pico if you
omit it). That one command:

- created `board/src/pico_ext_led.rs` — an exact clone of the Pico's manifest;
- added the `board-pico-ext-led` feature to `board` and `kernel`;
- registered the module in `board/src/lib.rs`;
- added it to the Makefile so `make flash` builds it for the RP2040.

The clone is **identical** to the Pico. Nothing works differently yet — which is
the point: now you change only the one fact that differs.

## 2. Change the one fact: the LED pin

Open `board/src/pico_ext_led.rs`. Near the top it declares the user LED, copied
from the Pico as GP25. Change it to GP16:

```rust ins={4}
pub const BOARD_NAME: &str =
    "Raspberry Pi Pico (external LED on GP16)";
// The user LED is an external LED on GP16, not the onboard GP25.
pub const USER_LED_GPIO: u8 = 16;
```

That's the whole change. Everything else in the file — the console UART, the
flash map, the buses — is the shared RP2040 base the clone re-exports, and none
of it differs on this board.

## 3. Flash Blinky at it — unchanged

The app never mentioned a pin, so it needs no edit. Flash the same Blinky,
naming the new board:

```bash
make flash APP=blinky BOARD=board-pico-ext-led
```

The LED on **GP16** blinks at 1 Hz, and the console logs `on`/`off` exactly as
before. The onboard GP25 LED stays dark — the app is driving the pin the
manifest named.

## What the command wired for you

Cloning a board by hand means editing a handful of files in step. `make
new-board` does each one, so a scaffolded board builds and flashes immediately:

| File | What it adds |
|---|---|
| `board/src/<name>.rs` | the manifest, cloned from the template |
| `board/Cargo.toml` | the `board-<name>` feature |
| `kernel/Cargo.toml` | the same feature, forwarded |
| `board/src/lib.rs` | `pub mod`, `active` re-export, board count |
| `Makefile` | the board's target family (RP2040 here) |

You only edited the pin. The device accessors — `board::user_led()`,
`board::console()` — you never touched: they're written against the **driver
family**, not any one board, so every board in a family gets them for free.

## What the manifest bought you

| | Blinky on the Pico | Blinky on your board |
|---|---|---|
| The app | drives "the user LED" | **unchanged** |
| The LED pin | GP25 (onboard) | GP16 (external) |
| What changed | — | `make new-board`, then one line |

The app asked the board a question — *which pin is the user LED?* — instead of
answering it itself. That's why moving the LED was a board change, not an app
change, and why the same Blinky runs on both.

## Next

You've now met all three layers a FlintOS program stands on: an **app**
(Blinky), the **kernel** that runs it, and a **board** that maps its requests
to real pins. From here, the [Users](/users/quickstart/) guide covers the
supported boards and flashing in depth, and the
[Developers](/developers/architecture/) guide goes under the manifest into the
driver layers it rests on.
