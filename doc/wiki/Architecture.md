# Architecture

Two independent stacks: how hardware is described, and how drivers are layered.

## Hardware: arch / SoC / board

Three things that vary independently, so three tiers.

| Tier | Owns | Example |
|---|---|---|
| `arch/xtensa` | **CPU** — traps, context switch, tick, register model | [Xtensa LX6](Arch-Xtensa-LX6) |
| `soc/esp32` | **Chip** — peripheral addresses, IRQ map, pin mux, clocks | [ESP32](SoC-ESP32) |
| `board/` | **PCB** — which pin is wired to what | [WROVER](Board-ESP32-WROVER) |

ESP32 and ESP32-S3 share neither peripheral map nor core. A WROVER and an
M5Stack Atom share both and differ only in wiring. Keeping them apart is what
makes adding a board one small file instead of a `cfg` tree.

### Why pin routing lives in the SoC tier

It has no portable implementation, only a portable contract:

| SoC | Model |
|---|---|
| ESP32 | GPIO matrix — almost any signal to almost any pad |
| STM32 | Alternate functions — a fixed short list per pad |
| NXP / i.MX | IOMUXC — a third model |

Drivers call `PinMux::route(signal, pin, config)`. Each SoC crate answers in its
own idiom, or returns `InvalidConfig` where the silicon genuinely can't comply.
Board manifests declare pins and nothing else.

## Drivers: three layers

```
   your app          ┌─────────────────────────────────────────┐
   ──────────        │  task    task    task    task           │  ← you write these
                     └────────────────┬────────────────────────┘
                                      │  flint-api
   Layer 3           ┌────────────────┴────────────────────────┐
   logical drivers   │  bme280        ssd1306      your_device │  ← portable across MCUs
                     └────────────────┬────────────────────────┘
   Layer 2           ┌────────────────┴────────────────────────┐
   bus abstraction   │  spi-bus       i2c-bus      uart-bus    │
                     └────────────────┬────────────────────────┘
   Layer 1           ┌────────────────┴────────────────────────┐
   physical drivers  │  esp32-spi     esp32-i2c    esp32-uart  │  ← portable across devices
                     └─────────────────────────────────────────┘
```

- **Layer 3** knows a device, not a chip. Depends only on `flint-api`.
- **Layer 2** knows a protocol, not registers.
- **Layer 1** knows registers. Depends on `flint-hal` and its SoC crate.

New sensor → write only Layer 3. New MCU → write only Layer 1.

**This is enforced.** `tools/check-layers.sh` fails the build if a Layer 2 or 3
crate names `flint-hal`, `flint-arch-*` or `flint-soc-*`. It runs in CI.

## Repository

```
apps/                  applications — the binaries you flash
hal/                   traits + types everything depends on (depends on nothing)
api/                   the API your application uses
arch/xtensa/           CPU
soc/esp32/             chip
kernel/                scheduler, IPC, timers, IRQ routing, debug — a library
board/                 PCB
drivers/physical/      Layer 1
drivers/bus/           Layer 2
drivers/logical/       Layer 3
tools/                 build helper, size report, layer check
```

Directory names carry no `flint-` prefix; package names do. A package name is
global and has to be unambiguous on crates.io; a directory is already scoped by
its path.

## The kernel is a library

The binary is an application in `apps/`. See
[Writing an Application](Writing-an-Application).

That's why board and debug features are selected by the app: it's the only crate
that can choose without the choice leaking into everything else that links the
kernel.
