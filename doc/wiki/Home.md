# FlintOS

A preemptive real-time OS for **32-bit** microcontrollers, in `no_std` Rust.

```bash
git clone https://github.com/cooljackal/flintos
cd flintos
make flash
```

Three tasks running on an ESP32. No Kconfig, no CMake, no vendor SDK.

## Status

Pre-alpha. Boots, schedules and preempts on real silicon (ESP32-PICO), on
both cores. Young, most drivers thin, API will change. Don't ship it.

32-bit only — see [Architecture](Architecture#word-size).

## Start here

| Page | What it covers |
|---|---|
| [Getting Started](Getting-Started) | Toolchain, first flash, reading the output |
| [Writing an Application](Writing-an-Application) | Your own tasks |
| [Upgrading](Upgrading) | `make upgrade` — what a pull broke |
| [Troubleshooting](Troubleshooting) | It didn't work |

## Reference

| Page | What it covers |
|---|---|
| [Architecture](Architecture) | How the layers fit |
| [Multicore](Multicore) | Both cores, one scheduler; pinning tasks |
| [Debug Levels](Debug-Levels) | What each level costs |
| [Writing a Driver](Writing-a-Driver) | One page per layer |
| [Adding a Board](Adding-a-Board) | One file |

## Hardware

Pin tables and register maps, so you don't have to go looking.

| Page | What it covers |
|---|---|
| [Xtensa LX6](Arch-Xtensa-LX6) | The CPU: windows, traps, tick |
| [ESP32](SoC-ESP32) | The chip: every pin, every peripheral |
| [ESP32-DevKitC / WROOM-32](Board-ESP32-DevKitC) | Board pinout — 🟢 the verified board |
| [M5Stack Atom](Board-M5Stack-Atom) | Board pinout — 🟢 verified |
| [ESP32-WROVER](Board-ESP32-WROVER) | Board pinout — 🟡 the default, never flashed |

🟢 checked on real silicon · 🟡 manifest written, nobody has flashed it.
`make flash` with no `BOARD=` builds for the WROVER; the tested path is
`BOARD=board-esp32-devkitc`.

## Elsewhere

- [Issues](https://github.com/cooljackal/flintos/issues) — the source of truth for open work
- [`doc/review-findings.md`](https://github.com/cooljackal/flintos/blob/main/doc/review-findings.md) — the adversarial review that shaped the tree
- [`doc/plan-radio.md`](https://github.com/cooljackal/flintos/blob/main/doc/plan-radio.md) — how Wi-Fi and BLE would be added, and what they cost
