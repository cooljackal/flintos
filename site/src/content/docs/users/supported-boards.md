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
| M5Stack Core2 | Xtensa LX6 / ESP32-D0WDQ6 | `board-m5-core2` | 🟠 bring-up in progress — power rails, MPU6886 IMU, ILI9342C LCD and FT6336U touch verified on hardware |
| Wio RP2040 Mini | ARMv6-M / RP2040 | `board-wio-rp2040-mini` | 🟢 verified — kernel suite, both cores |
| Raspberry Pi Pico | ARMv6-M / RP2040 | `board-raspberry-pi-pico` | 🟢 verified — kernel + peripheral suite over SWD |
| [ESP32-WROVER](/hardware/board-esp32-wrover/) | Xtensa LX6 / ESP32 | `board-esp32-wrover` | 🟡 manifest written, never flashed |

🟢 checked on real silicon · 🟠 bring-up in progress, some subsystems verified · 🟡 should work, nobody has flashed it

```bash
make flash BOARD=board-esp32-devkitc
```

Enabling two boards is a compile error, not a warning — a wrong pin map looks
like broken hardware, so the build refuses to guess.

## What "verified" covers

- **Xtensa boards** — the full on-target self-test suite (`make test-target`),
  flash round-trip, and both cores scheduling.
- **RP2040 boards** — the ARMv6-M kernel suite (boot, preemption, context switch,
  critical sections, queues, faults, both cores) **and** the peripheral drivers:
  GPIO, UART, SPI, I²C, ADC and conditioned entropy, DMA, PIO, flash KV, native
  USB CDC, per-core MPU task isolation, and the measured CPU clock — each with an
  on-target acceptance test driven over SWD (`make test-arm-*`), including PWM
  frequency and duty measured through a physical GP2→GP3 loopback.

RP2040 boards flash over USB with no debug probe — `make flash` enters BOOTSEL
automatically (a 1200bps touch) when the board already runs FlintOS with USB
enabled, or you hold **BOOTSEL** for the first flash.

The 🟡 WROVER row is the honest one: its tests pass but no one has held the
board. **Flashing a board we haven't is the contribution we want most** — a
garbled serial dump is a useful result.

## Adding your board

One manifest under `board/src/`, plus a feature line in a few `Cargo.toml`
files. See [Adding a Board](/developers/adding-a-board/). The per-chip pin and register
detail you'll need lives in [ESP32](/hardware/soc-esp32/) and [Xtensa LX6](/hardware/arch-xtensa-lx6/).
