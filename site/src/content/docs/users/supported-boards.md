---
title: Supported boards
---


A board is one manifest file — the pin map, bus map and IRQ numbers. Pick the
feature that matches your hardware; `BOARD` has no default.

| Board | Arch / SoC | Feature | Status |
|---|---|---|---|
| [ESP32-DevKitC / WROOM-32](/hardware/board-esp32-devkitc/) | Xtensa LX6 / ESP32 | `board-esp32-devkitc` | 🟢 verified — the reference board |
| [M5Stack Atom Matrix](/hardware/board-m5stack-atom/) | Xtensa LX6 / ESP32-PICO | `board-m5-atom-matrix` | 🟢 verified — LED panel, IMU, ADC |
| [M5Stack Atom Lite](/hardware/board-m5stack-atom/) | Xtensa LX6 / ESP32-PICO | `board-m5-atom-lite` | 🟢 verified — one LED |
| Wio RP2040 Mini | ARMv6-M / RP2040 | `board-wio-rp2040-mini` | 🟢 verified — kernel suite, both cores |
| [ESP32-WROVER](/hardware/board-esp32-wrover/) | Xtensa LX6 / ESP32 | `board-esp32-wrover` | 🟡 manifest written, never flashed |

🟢 checked on real silicon · 🟡 should work, nobody has flashed it

```bash
make flash BOARD=board-esp32-devkitc
```

Enabling two boards is a compile error, not a warning — a wrong pin map looks
like broken hardware, so the build refuses to guess.

## What "verified" covers

- **Xtensa boards** — the full on-target self-test suite (`make test-target`),
  flash round-trip, and both cores scheduling.
- **Wio RP2040 Mini** — the ARMv6-M kernel suite: boot, preemption, context
  switch, critical sections, queues, faults, and both RP2040 cores. Peripheral
  drivers are Xtensa-only for now.

The 🟡 WROVER row is the honest one: its tests pass but no one has held the
board. **Flashing a board we haven't is the contribution we want most** — a
garbled serial dump is a useful result.

## Adding your board

One manifest under `board/src/`, plus a feature line in a few `Cargo.toml`
files. See [Adding a Board](/developers/adding-a-board/). The per-chip pin and register
detail you'll need lives in [ESP32](/hardware/soc-esp32/) and [Xtensa LX6](/hardware/arch-xtensa-lx6/).
