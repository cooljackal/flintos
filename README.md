<!-- SPDX-License-Identifier: Apache-2.0 -->

```
   ███████╗██╗     ██╗███╗   ██╗████████╗ ██████╗ ███████╗
   ██╔════╝██║     ██║████╗  ██║╚══██╔══╝██╔═══██╗██╔════╝
   █████╗  ██║     ██║██╔██╗ ██║   ██║   ██║   ██║███████╗
   ██╔══╝  ██║     ██║██║╚██╗██║   ██║   ██║   ██║╚════██║
   ██║     ███████╗██║██║ ╚████║   ██║   ╚██████╔╝███████║
   ╚═╝     ╚══════╝╚═╝╚═╝  ╚═══╝   ╚═╝    ╚═════╝ ╚══════╝
```

**A preemptive real-time OS for 32-bit microcontrollers, in `no_std` Rust.**

No Kconfig. No CMake. No vendor SDK. No POSIX pretense. `git clone` →
`make flash BOARD=<your board>` → three tasks running on real silicon.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![CI](https://github.com/cooljackal/flintos/actions/workflows/ci.yml/badge.svg)](https://github.com/cooljackal/flintos/actions/workflows/ci.yml)
[![Status](https://img.shields.io/badge/status-pre--alpha-orange.svg)](#status)

---

## Status

**Pre-alpha. Don't ship it.** FlintOS boots, schedules and preempts on real
silicon — Xtensa (ESP32-WROOM, ESP32-PICO) and ARM (Wio RP2040 Mini) — but it
is young, most drivers are thin, and the API will change.

Wi-Fi runs as a station: it scans, associates, and completes a WPA2-PSK
handshake in a first-party Rust supplicant, then brings up an IP stack (smoltcp)
over the link — a DHCP lease, DNS resolution and an outbound TCP connection, on
hardware ([#68]). BLE is groundwork only. See
[doc/plan-radio.md](doc/plan-radio.md).

Full docs are at [flintos.dev][wiki], the API reference is at [flintos.dev/api][apidocs],
and open work lives in [issues] — the source of truth.

✅ verified on hardware · 🧪 host-tested, not yet on silicon · 🚧 partial · ⛔ not started · — n/a

### Kernel

| Feature | Xtensa LX6 | ARM32 |
|---|---|---|
| Boots | ✅ | ✅ |
| Preemptive scheduling, 48 priorities | ✅ | ✅ |
| Context switch | ✅ | ✅ |
| Interrupts, nesting, critical sections | ✅ | ✅ |
| Peripheral interrupt routing | ✅ | ✅ |
| Tick timer, measured CPU clock | ✅ | ✅ |
| Mutexes with priority inheritance | ✅ | ✅ |
| Queues, task↔task and ISR→task | ✅ | ✅ |
| Task-vs-ISR race tests | ✅ | ✅ |
| Watchdogs | ✅ | ✅ |
| Reset-cause reporting | ✅ | ✅ |
| Logging, metrics, panic capture | ✅ | ✅ |
| Stack high-water marks | ✅ | ✅ |
| Second core · task pinning | ✅ | ✅ |
| DMA | ✅ | ✅ |

### Peripherals

| Peripheral | Xtensa LX6 | ARM32 |
|---|---|---|
| UART · GPIO · pin routing | ✅ | ✅ |
| I²C · SPI | ✅ | ✅ |
| PWM / LEDC · Timers (TIMG) | ✅ | ✅ |
| ADC | ✅ | ✅ |
| Second ADC † · DAC † | ✅ | — |
| RMT pulse generator | ✅ | — |
| Hardware RNG | ✅ | — |
| Flash key/value | ✅ | ✅ |
| CAN (TWAI) † · I2S † | ✅ | ⛔ |
| Wi-Fi · BLE | 🚧 | — |
| Touch · SD/SDIO · Ethernet MAC | ⛔ | ⛔ |
| USB | — | ⛔ |

† Verified on a DevKitC by an on-chip loopback self-test (no external hardware):
DAC→ADC2 readback, ADC2 refusing while the radio owns the SAR, TWAI self-receive,
I2S DMA through a one-pad loop. Not claimed: analog accuracy, a real CAN bus.

The RP2040 ADC was verified on a Pico with 1,024 internal-temperature samples.
RP2040 entropy is a conditioned best-effort seed from spaced ring-oscillator
samples; the chip has no cryptographic hardware RNG, and FlintOS does not claim one.

RP2040 buses were verified on a Pico: 4,096 SPI internal-loopback bytes and
1,001 wired I²C write/repeated-start/read transactions (8,008 payload bytes),
including recovery after a missing-device response and forced timeouts on both
controllers. These are polled drivers; SPI chip-select, peripheral DMA and I²C
address-only scans are not provided. See [bus acceptance](doc/rp2040-bus-acceptance.md).

RP2040 flash/key-value storage was verified on the Pico in a reserved 16 KiB
partition, including writes from both cores, reset persistence, torn-tail
recovery and automatic watchdog recovery while flash execution is disabled.
Existing whole-store compaction is not power-loss atomic. See
[flash acceptance](doc/rp2040-flash-acceptance.md) before running the destructive test.

### Device drivers

Real parts you attach — one part number each, MCU-agnostic (`drivers/logical/`).

| Device | What it is | Status |
|---|---|---|
| MPU6886 | 6-axis IMU (accelerometer + gyro) | ✅ |
| WS2812 / SK6812 | Addressable RGB LED | ✅ |
| AXP192 | Power-management IC — rails + battery status | ✅ |
| BMI270 | 6-axis IMU | 🧪 |
| BME280 | Temperature / humidity / pressure sensor | 🧪 |
| SSD1306 | 128×64 monochrome OLED display | 🧪 |
| FT6336U | Capacitive touchscreen controller | 🧪 |

### Logical drivers

No hardware of their own — pure code over a device contract (`lib/`), portable to
any part that keeps the contract. See [Libraries](https://flintos.dev/developers/libraries/).

| Driver | What it is | Status |
|---|---|---|
| LED panel geometry | `(x, y)` → LED-chain index mapper (`led-matrix`) | ✅ |
| LED strip effects | Effects over the `LedStrip` contract (`led-strip`) | ✅ |

### Build and test

| Check | Status |
|---|---|
| Host unit tests | ✅ 925 passing, kernel included — `make test-host` |
| On-target self-tests | ✅ 32 pass, 1 skip on a WROOM — `make test-target` |
| Layer boundary + package naming | ✅ enforced in CI |
| Image size, per region | `make size` |
| ABI versioning + upgrade path | `make upgrade` |

---

## Supported boards

| Board | Arch / SoC | Status |
|---|---|---|
| ESP32-DevKitC / WROOM-32 | Xtensa LX6 / ESP32 | ✅ verified — reference board; rev 1 & rev 3, full self-test suite + Wi-Fi/IP stack |
| M5Stack Atom Matrix | Xtensa LX6 / ESP32-PICO | ✅ verified — LED panel, IMU, ADC |
| M5Stack Atom Lite | Xtensa LX6 / ESP32-PICO | ✅ verified — one LED |
| M5Stack Core2 | Xtensa LX6 / ESP32-D0WDQ6 | 🟡 bring-up in progress — boots on AXP192 power rails + battery status; onboard IMU and FT6336U touch wired; LCD pending |
| Wio RP2040 Mini | ARMv6-M / RP2040 | ✅ verified — kernel suite, both cores |
| ESP32-WROVER | Xtensa LX6 / ESP32 | 🟡 manifest written, never flashed |

Full list, pinouts and register maps in the [docs][boards]. Adding a board is one
manifest file — see [Adding a Board][add-board].

**What we want most:** flash the 🟡 row, or any board not listed, and tell us
what happened. A garbled serial dump is a useful result — bringing up the
WROOM-32 found two real bugs in a day.

---

## Quickstart

```bash
git clone https://github.com/cooljackal/flintos
cd flintos
make flash BOARD=board-esp32-devkitc
```

That builds `apps/examples/demo`, flashes over USB, and opens a monitor.
Xtensa needs Espressif's Rust fork first — `cargo install espup espflash &&
espup install`, then `. $HOME/export-esp.sh` (`export-esp.ps1` on Windows).
Full setup and the "what a healthy boot looks like" walkthrough:
[Quickstart][quickstart].

**`BOARD` is required, no default.** A wrong pin map looks like a broken board,
not a build error, so `make flash` with no board lists them and stops.

```bash
make apps                                          # what you can flash
make flash APP=hello BOARD=board-esp32-devkitc     # minimal one-task template
make flash APP=imu   BOARD=board-m5-atom-matrix    # onboard IMU over I²C
make flash APP=smp   BOARD=board-m5-atom-matrix    # starts the second core
```

`DEBUG` defaults to `debug-level-1` (dev). `debug-level-0` compiles logging out
entirely. Board features: `board-esp32-devkitc`, `board-m5-atom-lite`,
`board-m5-atom-matrix`, `board-m5-core2`, `board-wio-rp2040-mini`,
`board-esp32-wrover`.

---

## Writing a task

A whole application, `apps/examples/hello/src/main.rs`:

```rust
#![no_std]
#![no_main]

use api::prelude::*;

kernel::flint_app!(main, abi = 2);

fn main() {
    // runs once, after the kernel is up but before interrupts unmask
    Task::new("blink", blink).spawn().expect("spawn");
}

fn blink() {
    loop {
        log_info!("tick at {} ms", timer::now_ms());
        sleep_ms(500);
    }
}
```

`Task::new(name, entry)` needs only those two; `.priority(..)`, `.stack(..)` and
`.on_core(..)` are optional builder steps before `.spawn()`, which returns `None`
if the task pool is full.

Priorities are banded — `Critical`, `Normal`, `Background`, each `0..15` — so you
slot a task in without renumbering. Queues (`api::queue`) are typed and bounded,
with an ISR-safe `send_isr()`; mutexes (`api::mutex`) carry priority inheritance.
Full tutorial: [Tutorial: Hello World][tutorial].

---

## What it is / isn't

- **Preemptive from line one** — 48 priorities, round-robin within a level,
  priority inheritance, 1 ms tick. Not a cooperative loop that grew a scheduler.
- **A three-layer driver model, enforced in CI** — a new sensor is *only* a
  device driver; a new peripheral is *only* a register driver. A lint fails the
  build if the two ever touch. See [Architecture][arch].
- **Debugging at zero release cost** — levelled logging, metrics, high-water
  marks and postmortem capture, all compiled out when the feature is off.
- **Not POSIX, not memory-isolated, not 64-bit.** One protection domain, tasks
  cooperatively trusted; 32-bit only, by design and for good.

**Skip it if** you need memory isolation between untrusted tasks, a network
stack or filesystem today, or something production-proven — reach for
[FreeRTOS](https://www.freertos.org), [Zephyr](https://zephyrproject.org) or
[Embassy](https://embassy.dev).

---

## Project layout

```
apps/examples/       applications — the binaries you flash; read in order
apps/tests/          on-target verification apps — PASS/FAIL, one issue each
hal/                 traits + types every layer depends on (depends on nothing)
api/                 the API your application code uses
arch/<core>/         CPU — boot, vectors, context switch, tick (xtensa, armv6m)
soc/<chip>/          chip — address map, pin mux, IRQ crossbar (esp32, rp2040)
kernel/              scheduler, IPC, timers, IRQ routing, debug — a library
board/               PCB — which pin is wired to what
drivers/physical/    Layer 1 — one peripheral's registers each, grouped by chip
drivers/bus/         Layer 2 — transport abstractions
drivers/logical/     Layer 3 — one part number each, MCU-agnostic
lib/                 portable libraries — no registers, no part numbers
tools/               build helpers, size report, layer/name checks
site/                docs site (Astro/Starlight), published to flintos.dev
doc/                 design notes, plans, and the migrated wiki source
```

arch / SoC / board are three tiers because the core, the chip and the circuit
board vary independently. `lib/` is not `drivers/`: a driver knows a part number
and drives a pin, a lib touches no hardware at all. Details in
[Architecture][arch].

## Development

```bash
make               # list every target, with what it does
make test-host     # unit tests, every crate, no hardware
make lint          # clippy, warnings denied
make check-layers  # three-layer boundary enforcement
make size          # where the image's bytes went, per region
```

Before every commit: `make test-host && make lint && make check-layers`.
See [CONTRIBUTING.md](CONTRIBUTING.md) — commits need a DCO sign-off
(`git commit -s`).

## License

[Apache-2.0](LICENSE). Patent grant included — see section 3.

[wiki]: https://flintos.dev
[apidocs]: https://flintos.dev/api/
[issues]: https://github.com/cooljackal/flintos/issues
[boards]: https://flintos.dev/users/supported-boards/
[quickstart]: https://flintos.dev/users/quickstart/
[tutorial]: https://flintos.dev/users/hello-world/
[arch]: https://flintos.dev/developers/architecture/
[add-board]: https://flintos.dev/developers/adding-a-board/
[#68]: https://github.com/cooljackal/flintos/issues/68
