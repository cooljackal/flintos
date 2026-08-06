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

### Breaking

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
