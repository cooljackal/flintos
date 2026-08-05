<!-- SPDX-License-Identifier: Apache-2.0 -->

```
   ███████╗██╗     ██╗███╗   ██╗████████╗ ██████╗ ███████╗
   ██╔════╝██║     ██║████╗  ██║╚══██╔══╝██╔═══██╗██╔════╝
   █████╗  ██║     ██║██╔██╗ ██║   ██║   ██║   ██║███████╗
   ██╔══╝  ██║     ██║██║╚██╗██║   ██║   ██║   ██║╚════██║
   ██║     ███████╗██║██║ ╚████║   ██║   ╚██████╔╝███████║
   ╚═╝     ╚══════╝╚═╝╚═╝  ╚═══╝   ╚═╝    ╚═════╝ ╚══════╝
```

**A preemptive real-time OS for microcontrollers, in `no_std` Rust.**

No Kconfig. No CMake. No vendor SDK. No POSIX pretense. `git clone` → `make flash`
→ three tasks running on an ESP32.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Status](https://img.shields.io/badge/status-pre--alpha-orange.svg)](#status)
[![Target](https://img.shields.io/badge/target-ESP32%20(Xtensa%20LX6)-lightgrey.svg)](#supported-hardware)

---

## Status

**Pre-alpha. Do not put this in a product.** Flint compiles, links a real Xtensa
image, and passes its host test suite — but the boot and context-switch path has
**not yet been verified on silicon**, and several ESP32 peripheral drivers have
known-wrong register offsets.

| | |
|---|---|
| Builds for `xtensa-esp32-none-elf` | ✅ links a 87 KB image |
| Host unit tests | ✅ 49/49 passing |
| Three-layer boundary enforced in CI | ✅ |
| Boots on real hardware | ⚠️ **unverified** |
| Preemption proven on silicon | ⚠️ **unverified** |
| UART / GPIO / SPI / I²C drivers | ⚠️ **register audit in progress** |

An adversarial review of the whole tree is tracked in
[`doc/review-findings.md`](doc/review-findings.md). Nothing here is hidden: if a
driver is a stub, it says so.

**What we want from you right now:** flash it to a board, tell us what happens.

---

## What it is

Flint is a small preemptive RTOS built around three ideas:

1. **A scheduler that preempts from the first line of code.** 48 priority levels,
   round-robin inside a level, priority inheritance on mutexes, 1 ms tick. Not a
   cooperative loop that grew a scheduler later.
2. **A three-layer driver model that is enforced, not suggested.** Adding a new
   sensor means writing *only* the device layer. Adding a new MCU means writing
   *only* the register layer. A CI check fails the build if the two ever touch.
3. **Debugging as a first-class feature, at zero release cost.** Levelled logging,
   metrics, stack high-water marks, and postmortem capture — all compiled out
   entirely when you turn the feature off.

## What it isn't

- **Not POSIX.** No `fork`, no `pthread`, no libc shim. The API fits the hardware.
- **Not memory-isolated.** Flint runs in a *single protection domain* — all tasks
  share one address space with no MPU enforcement. Tasks are cooperatively
  trusted. If you need hardware isolation between untrusted components, Flint is
  the wrong tool and may always be.
- **Not multicore.** The ESP32's second core is currently unused.

---

## How it works (30 seconds)

```
   your app          ┌─────────────────────────────────────────┐
   ──────────        │  task    task    task    task           │  ← you write these
                     └────────────────┬────────────────────────┘
                                      │  flint-api
   Layer 3           ┌────────────────┴────────────────────────┐
   logical drivers   │  bme280        ssd1306      your_device │  ← portable across MCUs
                     └────────────────┬────────────────────────┘
   Layer 2           ┌────────────────┴────────────────────────┐
   bus abstraction   │  spi_bus       i2c_bus      uart_bus    │
                     └────────────────┬────────────────────────┘
   Layer 1           ┌────────────────┴────────────────────────┐
   physical drivers  │  esp32_spi     esp32_i2c    esp32_uart  │  ← portable across devices
                     └────────────────┬────────────────────────┘
                     ┌────────────────┴────────────────────────┐
   kernel            │  scheduler · IPC · timers · IRQ router  │
                     └────────────────┬────────────────────────┘
                     ┌────────────────┴────────────────────────┐
   arch              │  flint-arch-xtensa  (trap, tick, ctx)   │
                     └─────────────────────────────────────────┘
```

The boundary between Layer 3 and Layer 1 is the whole point. A `bme280` driver
depends only on `flint-api` — it cannot name an ESP32 register even if it wants
to, because `tools/check-layers.sh` fails the build if it tries.

---

## Get started

### 1. Install the toolchain (~5 minutes)

Flint targets Xtensa, which needs Espressif's Rust fork. `espup` handles it:

```bash
cargo install espup espflash
espup install
```

Then activate the environment in your shell:

```bash
. $HOME/export-esp.sh
```

<details>
<summary>Windows PowerShell</summary>

```powershell
. $env:USERPROFILE\export-esp.ps1
```
</details>

### 2. Build

```bash
git clone https://github.com/cooljackal/flintos
cd flintos
cargo +esp build --target xtensa-esp32-none-elf -Z build-std=core,compiler_builtins --features debug-level-1
```

`debug-level-1` turns on logging, which is what makes the demo tasks visible.

### 3. Flash and watch

```bash
espflash flash target/xtensa-esp32-none-elf/debug/flint-kernel --baud 921600 --monitor
```

Expected output:

```
[FLINT] FlintMain reached
Flint RTOS booting...
[kernel] Flint RTOS boot complete, entering idle
[sensor] reading #1 tick=500
[consumer] processing tick=1000
[sensor] reading #2 tick=1000
[housekeep] alive tick=3000
```

Three tasks at three priorities, interleaving on a 1 ms tick. If you see that,
preemption works.

> **If you don't see that** — that is the single most useful bug report you can
> file right now. Open an issue with your board model and the raw serial output,
> garbled or not.

---

## Writing a task

```rust
use flint_api::{task, timer, Priority};

fn blink() {
    loop {
        flint_api::log_info!("tick at {} ms", timer::now_ms());
        task::sleep_ms(500);
    }
}

task::spawn("blink", blink, Priority::Normal(1), 4096);
```

Priorities are banded — `Critical(0..15)`, `Normal(0..15)`, `Background(0..15)` —
so you can slot a task in without renumbering everything else.

### Talking between tasks

```rust
use flint_api::queue::{self, Queue};

static READINGS: Queue<u32, 8> = Queue::new();

fn producer() {
    loop {
        queue::send(&READINGS, 42, 100).ok();   // 100 ms timeout
        task::sleep_ms(1000);
    }
}

fn consumer() {
    loop {
        if let Ok(v) = queue::recv(&READINGS, 5000) {
            flint_api::log_info!("got {}", v);
        }
    }
}
```

Queues are typed, bounded, and `send_isr()` is safe to call from an interrupt
handler.

### Sharing state

```rust
use flint_api::mutex::{self, Mutex};

static COUNTER: Mutex<u32> = Mutex::new(0);

fn bump() {
    let mut guard = mutex::lock(&COUNTER);
    *guard += 1;
}   // released on drop, with priority inheritance unwound
```

---

## Supported hardware

| Board | SoC | Status |
|---|---|---|
| ESP32-WROVER | ESP32 (Xtensa LX6) | Board manifest present, bring-up in progress |
| ESP32-DevKitC / WROOM-32 | ESP32 | Should work — same manifest, untested |
| M5Stack Atom (Lite/Matrix) | ESP32-PICO-D4 | Manifest needed (different pin map) |
| STM32F4 (Cortex-M4) | ARM32 | Planned, not started |

Adding a board is one file. Copy `board/src/esp32_wrover.rs`, change the pin
numbers and base addresses, and register it in `board/src/lib.rs`.

---

## When to use Flint · When to skip it

**Reach for Flint when** you want a preemptive scheduler and typed IPC without
adopting an entire vendor ecosystem; when you value being able to read the whole
kernel in an afternoon; when you're writing device drivers you'd like to keep
when you change MCU.

**Skip Flint when** you need memory isolation between untrusted tasks (no MPU
enforcement, possibly ever); when you need a network stack, filesystem, or BLE
today (none exist yet); when you need something production-proven — use
[FreeRTOS](https://www.freertos.org), [Zephyr](https://zephyrproject.org), or
[Embassy](https://embassy.dev) instead; when you need multicore.

---

## Project layout

```
flint-hal/                 traits + types every layer depends on (depends on nothing)
flint-api/                 the API your application code uses
arch/flint-arch-xtensa/    boot, vectors, context switch, tick for Xtensa LX6
kernel/                    scheduler, IPC, timers, IRQ routing, debug
board/                     per-board manifests (pins, base addresses, devices)
drivers/physical/          Layer 1 — MCU register drivers
drivers/bus/               Layer 2 — transport abstractions
drivers/logical/           Layer 3 — device drivers, MCU-agnostic
tools/check-layers.sh      enforces the Layer 3 → Layer 1 boundary
```

~5,400 lines of Rust and ~570 lines of Xtensa assembly across 14 crates.

## Development

```bash
make check         # host-side compile check
make test-host     # 49 host unit tests
make lint          # clippy, warnings denied
make check-layers  # three-layer boundary enforcement
make build         # Xtensa build (needs the esp toolchain)
```

Debug features are additive and compile out entirely:
`debug-level-0` (silent) through `debug-level-3` (full trace).

## Roadmap

- **Now** — hardware bring-up on ESP32: prove the trap handler, context switch,
  and tick on real silicon.
- **Next** — driver register audit against the TRM; M5Stack Atom board support.
- **Later** — Layer-1 drivers as isolated tasks with one-IPC-hop request/reply;
  `nsh` shell; STM32F4 port.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Commits need a DCO sign-off (`git commit -s`).
The most valuable contribution today is a bug report from real hardware.

## License

[Apache-2.0](LICENSE). Patent grant included — see section 3.
