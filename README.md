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

**Pre-alpha. Do not put this in a product.** Flint builds, links a real Xtensa
image, and passes its host test suite — but **nothing here has been run on
silicon yet**, and one known defect in the context-switch path is still open.

| | |
|---|---|
| Builds for `xtensa-esp32-none-elf` | ✅ |
| Host unit tests | ✅ 99/99 passing |
| Three-layer boundary enforced in CI | ✅ |
| UART, GPIO, SPI register maps | ✅ audited against Espressif's headers |
| I²C driver | ⛔ `init()` rejects all configs — pin routing not implemented |
| Register-window spill on switch | ⛔ **missing** — see below |
| Boots on real hardware | ⚠️ **unverified** |
| Preemption proven on silicon | ⚠️ **unverified** |

**The one open blocker.** Xtensa keeps live call frames in a rotating register
file. When the scheduler switches away from a task, that task's outer frames
must be spilled to its stack, and Flint does not yet do this — the spill routine
is a stub that no code calls. Expect corruption for any task interrupted more
than one call deep. Everything else on the critical path (trap entry, register
save/restore, boot window state, memory map, console) has been repaired and
verified by disassembling the linked image.

Open work is tracked in
[issues](https://github.com/cooljackal/flintos/issues) — that is the source of
truth. The full adversarial review that produced them, including what was
checked and found sound, is in
[`doc/review-findings.md`](doc/review-findings.md). Nothing is hidden: where a
driver is a stub or a value is an assumption, it says so.

**What we want from you right now:** flash it to a board, tell us what happens —
[issue #15](https://github.com/cooljackal/flintos/issues/15) is the bring-up
gate, and a garbled serial dump is a useful result.

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

### 2. Build and flash

```bash
git clone https://github.com/cooljackal/flintos
cd flintos
make flash-dev
```

`flash-dev` builds with `debug-level-1` and flashes over USB serial.
**Use it rather than `make flash`** — logging is off by default, so `make flash`
boots correctly but prints no task output, which looks exactly like a kernel
that isn't scheduling.

Building for a board other than the WROVER default:

```bash
cargo +esp build -p flint-kernel --target xtensa-esp32-none-elf -Z build-std=core,compiler_builtins --features debug-level-1 --no-default-features --features board-m5-atom
```

Board features: `board-esp32-wrover` (default), `board-esp32-devkitc`,
`board-m5-atom` (ESP32-PICO-D4). Enabling two is a compile error, not a warning
— a silently wrong pin map presents as the board being broken.

<details>
<summary><b>If flashing fails</b></summary>

**`Error while connecting to device`, right after `Using flash stub`** — the
baud switch failed. This is the common one. Flashing defaults to 115200 for
that reason; if you raised it, put it back:

```bash
make flash-dev FLASH_BAUD=115200
```

**`Error while connecting to device`, before the stub** — the board isn't in
download mode. Most dev boards enter it automatically via DTR/RTS, but some
(and most USB hubs) don't. Hold **BOOT**/**GPIO0**, tap **EN**/**RST**, release
BOOT, then flash. On an M5Stack Atom the reset button is the small side button.

**Wrong or busy serial port** — close any other monitor first; only one process
can hold the port. Pass it explicitly with `--port COM5` (or `/dev/ttyUSB0`).

**A previous image is wedging the board** — erase and retry:

```bash
make erase
```

**The flash stub itself is the problem** — rare, but on some clones:
`espflash flash --no-stub ...`

Flashing succeeds but the console is garbage: that's a baud mismatch, not a
kernel fault. `--monitor-baud` must be 115200 to match the board manifest, and
it is a *different* flag from `--baud`.
</details>

### 3. What a healthy boot looks like

```
[FLINT] FlintMain reached (_start -> Rust OK)
[FLINT] VECBASE=0x40080000 _vector_table_start=0x40080000 MATCH
[FLINT] PS=0x0004000f WOE=1
[FLINT] SP=0x3ffb1f30 task_stack_pool=[0x3ffc0000, 0x3ffd8000)
[FLINT] startup::init done
[FLINT] cpu_hz=80000000 (measured)
[FLINT] tick period=80000 CCOUNT ticks
[FLINT] unmasking interrupts...
[FLINT] interrupts unmasked, entering idle
[sensor    prio=Normal(1)]  n=1 tick=500
[consumer  prio=Normal(5)]  n=1 tick=1000
[sensor    prio=Normal(1)]  n=2 tick=1000
[housekeep prio=Background(1)] n=1 tick=3000
```

Each banner line proves the step before it, so the **last line you see tells you
where it died**:

| Last line | What it means |
|---|---|
| *(nothing)* | Dead before Rust, or the console baud is wrong |
| `FlintMain reached` | The vector table didn't install — check `VECBASE` |
| `VECBASE ... MISMATCH` | Traps will go to ROM; nothing will schedule |
| `WOE=0` | Register windows off; every windowed call is unreliable |
| `cpu_hz=... (ASSUMED)` | Clock measurement failed; every timeout is scaled by a guess |
| `unmasking interrupts...` | **It died in its first-ever interrupt** — trap entry or `_flint_trap` |

If tasks appear but the timing is off by a constant factor, that's the CPU clock
([#6](https://github.com/cooljackal/flintos/issues/6)). If they run briefly and
then misbehave, that's most likely the missing window spill
([#1](https://github.com/cooljackal/flintos/issues/1)) — a genuine stack
overflow reports itself by name instead.

> **If it doesn't work, that is the single most useful thing you can report.**
> [Issue #15](https://github.com/cooljackal/flintos/issues/15) is the bring-up
> gate — post your board model and the raw serial output, garbled or not.

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
