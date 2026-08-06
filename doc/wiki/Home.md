# Flint RTOS

A preemptive real-time OS for microcontrollers, in `no_std` Rust.

```bash
git clone https://github.com/cooljackal/flintos
cd flintos
make flash
```

Three tasks running on an ESP32. No Kconfig, no CMake, no vendor SDK.

## Status

Pre-alpha. Boots, schedules and preempts on real silicon (ESP32-PICO). Young,
most drivers thin, API will change. Don't ship it.

## Start here

| Page | What it covers |
|---|---|
| [Getting Started](Getting-Started) | Toolchain, first flash, reading the output |
| [Writing an Application](Writing-an-Application) | Your own tasks |
| [Troubleshooting](Troubleshooting) | It didn't work |

## Reference

| Page | What it covers |
|---|---|
| [Architecture](Architecture) | How the layers fit |
| [Debug Levels](Debug-Levels) | What each level costs |
| [Writing a Driver](Writing-a-Driver) | One page per layer |
| [Adding a Board](Adding-a-Board) | One file |

## Hardware

Pin tables and register maps, so you don't have to go looking.

| Page | What it covers |
|---|---|
| [Xtensa LX6](Arch-Xtensa-LX6) | The CPU: windows, traps, tick |
| [ESP32](SoC-ESP32) | The chip: every pin, every peripheral |
| [ESP32-WROVER](Board-ESP32-WROVER) | Board pinout |
| [ESP32-DevKitC](Board-ESP32-DevKitC) | Board pinout |
| [M5Stack Atom](Board-M5Stack-Atom) | Board pinout |

## Elsewhere

- [Issues](https://github.com/cooljackal/flintos/issues) — the source of truth for open work
- [`doc/review-findings.md`](https://github.com/cooljackal/flintos/blob/main/doc/review-findings.md) — the adversarial review that shaped the tree
