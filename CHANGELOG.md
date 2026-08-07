<!-- SPDX-License-Identifier: Apache-2.0 -->

# Changelog

What changed, and — where it matters — what you have to do about it.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Flint
is pre-1.0 and moves fast, so **Breaking** is the section that earns this file
its place. Every entry there should say what to change, not only what changed.

Applications declare the ABI they were written against:

```rust
kernel::flint_app!(main, abi = 1);
```

A kernel that provides a different one refuses to build and points here.
`make upgrade` reports which of your applications an upgrade affects.

## [Unreleased]

### Fixed

- **The ESP32 I²C driver never returned the bytes it read.** `read` programmed
  the READ commands, waited for completion and left the data in the RX FIFO,
  returning `Ok(())`. Every I²C read this driver has ever done returned
  nothing — which is consistent with I²C never having been confirmed against a
  real device. It now takes a buffer and drains the FIFO.
- **The I²C address was shifted twice.** The bus layer pre-shifted in `write`
  and not in `transfer`, while the physical driver shifted again, so `0x76`
  reached the wire as `0xD8` and nothing would ACK. The convention — `tx[0]` is
  the 7-bit address, unshifted — is now written down on
  `hal::PhysicalBus::raw_transfer`, where both sides can see it.
- **A write-only or read-only I²C transfer did nothing and returned `Ok`.**
  `raw_transfer` acted only when both `tx` and `rx` were non-empty.
- **`I2cBus::read` addressed the general-call address**, sending a zeroed `tx`
  rather than the device address.

### Breaking

- **RMT, the watchdogs and the RNG moved out of `soc-esp32` into their own
  physical drivers.** A peripheral is something you write a driver for; the SoC
  crate holds what every driver needs underneath it.

  ```rust
  use esp32_rmt::{Entry, Rmt};   // was: soc_esp32::rmt::{Entry, Rmt}
  ```

  ```toml
  esp32-rmt = { path = "../../drivers/physical/esp32-rmt" }
  ```

  `kernel::rng` and `kernel::watchdog` are unchanged — the kernel re-exports
  them from the new crates.

- **`board-m5-atom` split into `board-m5-atom-lite` and
  `board-m5-atom-matrix`.** The Atom Lite has one LED and the Atom Matrix has a
  5×5 panel on the same pin, and one feature could not tell them apart — an
  application told only `RGB_LED_GPIO` drove the first LED of a panel and
  looked correct while 24 stayed dark.

  ```sh
  make flash APP=demo BOARD=board-m5-atom-matrix   # was: BOARD=board-m5-atom
  ```

  ```toml
  # in your application's Cargo.toml
  board-m5-atom-matrix = ["kernel/board-m5-atom-matrix"]
  ```

  The old name is still accepted and fails with a message naming the two
  replacements, rather than leaving cargo to say "does not contain this
  feature".

- **Applications must declare an ABI version.** `flint_app!(main)` no longer
  compiles.

  ```rust
  kernel::flint_app!(main, abi = 1);   // was: kernel::flint_app!(main);
  ```

  Without a declaration there is nothing to check, and an unversioned
  application is exactly the one that breaks silently on a kernel upgrade.

- **Every package lost its `flint-` prefix.** `flint-hal` → `hal`, `flint-api`
  → `api`, `flint-kernel` → `kernel`, `flint-arch-xtensa` → `arch-xtensa`,
  `flint-soc-esp32` → `soc-esp32`, `flint-board` → `board`.

  Update the `path` dependencies and the `use` statements in your application:

  ```toml
  kernel = { path = "../../kernel", default-features = false }
  api    = { path = "../../api" }
  hal    = { path = "../../hal" }
  ```

  ```rust
  use api::task;              // was: use flint_api::task;
  use hal::types::Priority;   // was: use flint_hal::types::Priority;
  ```

- **Directories were renamed to match**: `flint-hal/` → `hal/`,
  `arch/flint-arch-xtensa/` → `arch/xtensa/`, `soc/flint-soc-esp32/` →
  `soc/esp32/`, `drivers/**/esp32_uart/` → `drivers/**/esp32-uart/`, and
  `flint-build/` → `tools/build/`. Only affects you if you referenced a path
  directly.

- **The `phase0-tests` feature is now `self-test`.**

### Added

- **Watchdogs**, off unless an application opts in with
  `kernel::watchdog::arm()`. Two of them: the RTC watchdog is fed from the timer
  interrupt and catches a kernel that has stopped servicing it, and a
  timer-group watchdog is fed from the idle task and catches a task that never
  yields. Neither catches the other's failure — a spinning task keeps the tick
  alive, so only the idle-fed one notices.
- **`apps/blink`**, which drives the M5Stack Atom's onboard LED. It is also
  the on-hardware test for the RMT register map — no host test can tell you
  whether a register is where you think it is.
- **`tools/check-layers.sh` polices every tier**, not three of them: `hal`
  depends on nothing, `arch/*` and `soc/*` on `hal`, `drivers/physical/*` on
  `hal` and `soc/*`, bus and logical drivers on `api` and `lib/*`, `lib/*` on
  each other. 17 crates checked, up from 7 — `drivers/physical/` was entirely
  unchecked, so the layering could be inverted with CI green.
- **`#![forbid(unsafe_code)]` in every logical driver.** The dependency check
  cannot stop a driver writing to a register, because raw MMIO needs no
  dependency. This is the lint that makes the guarantee real.
- **`lib/led-strip`**: what an addressable LED strip promises, and effects
  written once against it rather than once per chip. `ws2812` implements
  `LedStrip`; it deliberately does not implement `Dimmable`, because these
  parts have no brightness register.
- **`make device-matrix`** prints which drivers keep which device-class
  promise, so "this chip cannot do it" and "nobody got round to it" stop
  looking identical.
- **`lib/`**, a home for portable libraries that are not drivers: no
  registers, no part numbers, output is a value rather than something bound for
  a pin. `tools/check-layers.sh` enforces that they depend only on `api` and on
  each other.
- **`led-matrix`** (in `lib/`): chained LED panel geometry, `(x, y)` to a
  position along the chain, with the fold described as data. It ships no board
  constants — a panel's layout is a fact about a board, so `board::active`
  declares it alongside the pin.
- **Board manifests declare their LEDs**: `RGB_LED_COUNT` and `RGB_LED_LAYOUT`
  join `RGB_LED_GPIO`, so an application no longer carries the count.
- **`make test-boards`** runs every board manifest's invariant tests. Only the
  selected board's tests ran before, leaving every other manifest unchecked.
- **Peripheral interrupt routing** (`soc_esp32::intr_map`). The DPORT crossbar
  that decides which of the CPU's 32 interrupt inputs a peripheral fires on.
  Nothing routed one before, so every driver's interrupt was unreachable.
- **RMT streaming** (`Rmt::start_stream`): frames longer than the 64-entry
  block, refilled half at a time from the channel's interrupt.
- **RMT feeds the channel through `RMTMEM` rather than the APB FIFO.** Via the
  FIFO, only the first frame transmitted: the write pointer is rewound by
  `APB_MEM_RST`, a different bit from the `MEM_RD_RST` that rewinds the read
  pointer, so every later frame landed past the terminator and the channel
  replayed the first one. An LED stuck on its first colour.
- **RMT driver** (`soc_esp32::rmt`) and a **WS2812/SK6812 logical driver**
  (`ws2812`), so an addressable LED can be driven with the sub-microsecond
  pulse timing it needs. One shot, one channel's memory block — about two LEDs;
  longer strings need refill-on-interrupt.
- **Hardware RNG** as `kernel::rng`. Suitable for backoffs, jitter and test
  seeds; **not** for keys or tokens — the generator is only cryptographically
  useful with the radio running, and Flint does not bring the radio up. Said
  plainly in the module docs rather than hidden behind a reassuring name.
- Six on-target tests for task-versus-ISR races, including a queue fed from the
  timer ISR and drained by a task.
- On-target self-test suite: `make test-target` flashes a board and turns the
  serial output into an exit code.
- Host tests for priority inversion and queue races, and `make test-host` now
  covers the kernel itself.
- `make size` — where an image's bytes went, per memory region.
- `make upgrade` — pull, rebuild every application, report which broke.
- GPIO-matrix pin routing (`PinMux`), so any signal reaches any pad.
- An `arch` / SoC / board split, and the wiki that documents each.

### Fixed

- Priority inheritance now follows a chain of blocked owners instead of one
  link, which is the difference between bounded and unbounded inversion.
- `Queue::send_isr` wakes a blocked receiver. It never did, so a driver task in
  `recv` slept forever with its data already in the ring.
- A task returning from its entry function no longer strands every other task
  at its priority level.
- Register windows are spilled before the trap entry moves the stack pointer.
  This was the long one.

[Unreleased]: https://github.com/cooljackal/flintos/compare/main...HEAD
