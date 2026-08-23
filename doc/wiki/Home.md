# FlintOS

A preemptive real-time OS for **32-bit** microcontrollers, in `no_std` Rust.

```bash
git clone https://github.com/cooljackal/flintos
cd flintos
make flash BOARD=board-esp32-devkitc
```

Three tasks running on real silicon. No Kconfig, no CMake, no vendor SDK.

## Status

Pre-alpha. Boots, schedules and preempts on Xtensa (ESP32-PICO, WROOM-32) and
ARM (Wio RP2040 Mini), both cores. Young, most drivers thin, API will change.
Don't ship it.

Wi-Fi runs as a station: it scans, associates, and completes a **WPA2-PSK
connection**, held to a real AP for minutes on hardware. The 4-way handshake is
a first-party Rust supplicant, not a vendored C one. No IP layer yet, so the
link drops at the AP's inactivity timeout. See
[`doc/plan-radio.md`](https://github.com/cooljackal/flintos/blob/main/doc/plan-radio.md).

32-bit only — see [Architecture](Architecture#word-size).

## Start here

| Page | What it covers |
|---|---|
| [Quickstart](Quickstart) | Toolchain, first flash, reading the output |
| [Supported Boards](Supported-Boards) | What runs where, and what "verified" means |
| [Tutorial: Hello World](Tutorial-Hello-World) | Your own tasks, from `apps/examples/hello` |
| [Writing a Driver](Writing-a-Driver) | Adding a driver, with examples per layer |

## Reference

| Page | What it covers |
|---|---|
| [API Overview](API-Overview) | The system + driver API, and the generated rustdoc |
| [Architecture](Architecture) | How the layers fit |
| [Multicore](Multicore) | Both cores, one scheduler; pinning tasks |
| [Debug Levels](Debug-Levels) | What each level costs |
| [Adding a Board](Adding-a-Board) | One file |
| [Libraries](Libraries) | `lib/` — contracts and pure code, no hardware |

## Hardware

Pin tables and register maps, so you don't have to go looking.

| Page | What it covers |
|---|---|
| [Xtensa LX6](Arch-Xtensa-LX6) | The CPU: windows, traps, tick |
| [ESP32](SoC-ESP32) | The chip: every pin, every peripheral |
| [ESP32-DevKitC / WROOM-32](Board-ESP32-DevKitC) | 🟢 the verified board |
| [M5Stack Atom](Board-M5Stack-Atom) | 🟢 verified |
| [ESP32-WROVER](Board-ESP32-WROVER) | 🟡 the default, never flashed |

## Elsewhere

- [Issues](https://github.com/cooljackal/flintos/issues) — the source of truth for open work
- [`doc/review-findings.md`](https://github.com/cooljackal/flintos/blob/main/doc/review-findings.md) — the adversarial review that shaped the tree
- [`doc/plan-radio.md`](https://github.com/cooljackal/flintos/blob/main/doc/plan-radio.md) — how Wi-Fi and BLE would be added, and what they cost
