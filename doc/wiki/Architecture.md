# Architecture

Two independent axes, which is the part that confuses people. **Layers 1–3 are
only about drivers.** `arch` / `soc` / `board` is a separate axis about how
specific a piece of hardware is. A crate has a position on one or the other,
never both.

## Word size

**32-bit only.** Not a limitation waiting to be lifted — an assumption the code
is built on:

| Where | What assumes it |
|---|---|
| `hal::types::TaskContext` | `#[repr(C)]` of `u32`s, indexed by fixed byte offset from `vectors.S`, size asserted at 96 |
| `arch/xtensa/flint32.ld` | Every region origin and length |
| `hal::bus::PhysicalBus` | Peripheral bases are `u32` |
| `soc/esp32` | The whole peripheral map, IO_MUX table and signal indices |
| `tools/size` | Parses ELF32 and rejects anything else |

A 64-bit port would rewrite the trap frame, the context switch and the memory
map rather than widen them. No microcontroller in this class needs it, so it is
not planned.

## Hardware: arch / SoC / board

Three things that vary independently, so three tiers.

| Tier | Owns | Example |
|---|---|---|
| `arch/xtensa` | **CPU core** — traps, context switch, tick, register model | [Xtensa LX6](Arch-Xtensa-LX6) |
| `soc/esp32` | **Chip infrastructure** — address map, IRQ crossbar, pin mux, clock gating | [ESP32](SoC-ESP32) |
| `board/` | **PCB** — which pin is wired to what, and what is on it | [Atom](Board-M5Stack-Atom) |

### What does *not* go in arch or soc

Only what is specific to that CPU or chip **and shared**. Anything that is one
peripheral is a driver, however chip-specific it is.

> **The test:** would a second peripheral driver need this?
> An address map and a pin router, yes — that is `soc/`.
> A pulse generator, no — that is `drivers/physical/`.

RMT, the watchdogs and the RNG were modules of `soc-esp32` until this rule was
written down; they are `drivers/physical/esp32/*` now. The ESP32 interrupt
crossbar was sitting in `arch/xtensa`, duplicated and dead, and was deleted.

ESP32 and ESP32-S3 share neither peripheral map nor core. A WROVER and an
M5Stack Atom share both and differ only in wiring. Keeping them apart is what
lets a new board be a manifest rather than a `cfg` tree through the kernel.

A manifest carries **every fact an application would otherwise look up in a
datasheet** — pins, addresses, IRQs, and the shape of whatever is attached. A
pin without the count of what is on it is half a fact: `RGB_LED_GPIO` alone let
an application drive one LED of a 25-LED panel and look correct while 24 stayed
dark. That is why the Atom is two boards, `-lite` and `-matrix`, declaring
`RGB_LED_COUNT` and `RGB_LED_LAYOUT` as well as the pin.

**Honest limit:** the directories and the dependency graph are in the right
shape for a second architecture, but the seam is not yet parameterised. There
is no arch-selection axis (`cfg(target_os = "none")` is equally true for a
Cortex-M target), `hal::TaskContext` is a set of Xtensa register-window fields,
and the kernel's fatal-fault path writes a hard-coded ESP32 UART address. A
second arch means building that axis first.

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
                                      │  api
   Layer 3           ┌────────────────┴────────────────────────┐        ┌──────────────┐
   logical drivers   │  bme280     ssd1306    ws2812           │───────▶│    lib/      │
                     └────────────────┬────────────────────────┘        │              │
   Layer 2           ┌────────────────┴────────────────────────┐───────▶│ led-strip    │
   bus abstraction   │  spi-bus       i2c-bus      uart-bus    │        │ led-matrix   │
                     └────────────────┬────────────────────────┘        │ (contracts + │
   Layer 1           ┌────────────────┴────────────────────────┐        │  pure code)  │
   physical drivers  │  esp32-spi  esp32-i2c  esp32-uart       │        └──────────────┘
                     │  esp32-rmt  esp32-wdt  esp32-rng        │
                     └────────────────┬────────────────────────┘
                     ┌────────────────┴────────────────────────┐
                     │  soc/esp32  — address map, pin mux, IRQ │
                     └────────────────┬────────────────────────┘
                     ┌────────────────┴────────────────────────┐
                     │  hal  — traits only, depends on nothing │
                     └─────────────────────────────────────────┘
```

- **Layer 3** knows a part number, not a chip.
- **Layer 2** knows a protocol, not registers.
- **Layer 1** knows one peripheral's registers.
- **`lib/`** is not a layer. See below.

New sensor → write only Layer 3. New MCU → write only Layer 1.

### Enforced, not suggested

`tools/check-layers.sh` is a whitelist per tier, and it runs in CI:

| Tier | May depend on |
|---|---|
| `hal` | nothing |
| `arch/*` | `hal` |
| `soc/*` | `hal` |
| `drivers/physical/*` | `hal`, `soc/*` |
| `drivers/bus/*` | `api`, `lib/*` |
| `drivers/logical/*` | `api`, `lib/*` |
| `lib/*` | other `lib/*` only |

**What a dependency check cannot do:** stop a driver writing to `0x3FF44008`.
Raw MMIO needs no dependency at all, so the graph never sees it — an
adversarial review demonstrated exactly that against `bme280`.
`#![cfg_attr(not(test), forbid(unsafe_code))]` in each logical driver is what
closes it. The two together are the guarantee; the dependency check alone never
was.

## `lib/` — not drivers at all

A driver knows a part number and its output is destined for a pin. `ws2812`
knows GRB order and 350 ns pulses; `bme280` knows a humidity register.

`lib/` crates know no chip, no bus and no pin, and return values instead.
`led-matrix` turns `(x, y)` into an integer and depends on nothing — not even
`api`. Filing that under `drivers/` would be a false statement about what it
is.

It holds two kinds of thing:

| Kind | Example |
|---|---|
| **Device-class contracts** | `trait LedStrip`, `trait Dimmable` |
| **Code generic over them** | hue wheel, gradients, panel geometry |

### Device classes: where a chip family generalises

Multiple chips do the same job. The contract is written once, above them:

```
lib/led-strip        trait LedStrip, trait Dimmable, effects
   ▲          ▲
ws2812     apa102    each keeps the promise its hardware can keep
```

The lib never learns a chip exists — the dependency points upward from the
driver. Adding a chip changes nobody else's code; changing chips is one line in
your application.

**The promises are deliberately small.** WS2812 has no brightness register, so
it implements `LedStrip` and not `Dimmable`. Under one fat trait it would have
to fake `set_brightness` by scaling the colour and losing depth, and a caller
could not tell that from an APA102 doing it properly in hardware.

So a missing `impl` is a real statement. To keep it from also meaning "nobody
got round to it", `make device-matrix` prints who keeps which promise:

```
  CONTRACT  FROM          IMPLEMENTED BY
  Dimmable  led-strip     (nobody yet)
  LedStrip  led-strip     ws2812
```

It reports and never fails — a chip that cannot do a thing must not break the
build for saying so.

**Write the contract when the second chip arrives, not the first.** With one
chip you are guessing what is common; with two you can see it. And a contract
written in the abstract goes wrong quietly: `PulseEmitter` had a single `emit`
method until a real panel used it, at which point it turned out to send 25
one-pixel frames instead of one, lighting only the first LED.

## Repository

```
apps/                  applications — the binaries you flash
hal/                   traits + types everything depends on (depends on nothing)
api/                   the API your application uses
arch/xtensa/           CPU
soc/esp32/             chip
kernel/                scheduler, IPC, timers, IRQ routing, debug — a library
board/                 PCB
drivers/physical/<soc>/  Layer 1 — one peripheral's registers each, by chip
drivers/bus/           Layer 2
drivers/logical/       Layer 3 — one part number each
lib/                   portable code — no registers, no part numbers
tools/                 build helper, size report, the three checkers
```

A package name is its directory leaf, unprefixed: `hal/` is `hal`,
`drivers/logical/bme280/` is `bme280`. Nothing here is published — every package
sets `publish = false` — so there is no global namespace to be unambiguous in,
and a prefix would only make every name longer.

## One scheduler, every core

There is one scheduler and one ready queue, shared by every core. A task is not
owned by a core; where it runs is a scheduling decision, and pinning it is a
constraint you add rather than the default.

This holds only for **symmetric** cores. See [Multicore](Multicore).

## The kernel is a library

The binary is an application in `apps/`. See
[Tutorial: Hello World](Tutorial-Hello-World).

That's why board and debug features are selected by the app: it's the only crate
that can choose without the choice leaking into everything else that links the
kernel.
