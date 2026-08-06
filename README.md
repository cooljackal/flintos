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

No Kconfig. No CMake. No vendor SDK. No POSIX pretense. `git clone` → `make flash`
→ three tasks running on an ESP32.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Status](https://img.shields.io/badge/status-pre--alpha-orange.svg)](#status)
[![Word size](https://img.shields.io/badge/word%20size-32--bit%20only-lightgrey.svg)](#what-it-isnt)
[![Target](https://img.shields.io/badge/target-ESP32%20(Xtensa%20LX6)-lightgrey.svg)](#supported-hardware)

---

## Status

**Pre-alpha. Do not put this in a product.** Flint boots, schedules and preempts
on real silicon — verified on an ESP32-PICO — but it is young, most drivers are
thin, and the API will change.

| What | Where it stands |
|---|---|
| Builds for `xtensa-esp32-none-elf` | ✅ |
| Host unit tests | ✅ 176 passing, kernel included |
| On-target self-tests | 11 tests — 5 verified on ESP32-PICO, 6 race tests added and not yet run on hardware |
| Layer boundary and package naming enforced in CI | ✅ |
| UART, GPIO, SPI register maps | ✅ audited against Espressif's headers |
| Boots on real hardware | ✅ ESP32-PICO, 80 MHz measured |
| Preemptive multitasking on silicon | ✅ three tasks, three priorities, timing exact |
| Register-window spill on switch | ✅ |
| GPIO-matrix pin routing | ✅ any signal to any pad, or a clear error |
| I²C driver | ⚠️ routes and configures; untested against a real device |
| Priority inversion | ✅ 11 host tests, including inheritance through a chain of blocked owners |
| Queue concurrency | ✅ 5 host tests on real threads — exactly-once delivery, contended slots, torn publish |
| Task-vs-ISR races | ⚠️ 6 on-target tests written, awaiting a board run ([#50](https://github.com/cooljackal/flintos/issues/50)) |
| Anything beyond ESP32 | ⛔ no Cortex-M port yet |

**Documentation:** the [wiki](https://github.com/cooljackal/flintos/wiki).

Open work is tracked in
[issues](https://github.com/cooljackal/flintos/issues) — that is the source of
truth. The full adversarial review that produced them, including what was
checked and found sound, is in
[`doc/review-findings.md`](doc/review-findings.md). Nothing is hidden: where a
driver is a stub or a value is an assumption, it says so.

**What we want from you right now:** flash it to a board that isn't an
ESP32-PICO and tell us what happens. A garbled serial dump is a useful result.

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
- **Not 64-bit, and not going to be.** Flint targets 32-bit parts. The
  assumption runs deeper than a few type aliases: the trap frame is a
  `#[repr(C)]` of `u32`s that `vectors.S` indexes by fixed byte offsets, the
  memory map and linker script are 32-bit throughout, and every driver takes a
  `u32` register base. Widening that is a rewrite of the context switch and the
  memory map, and no part in this class needs it.

---

## How it works (30 seconds)

```
   your app          ┌─────────────────────────────────────────┐
   ──────────        │  task    task    task    task           │  ← you write these
                     └────────────────┬────────────────────────┘
                                      │  api
   Layer 3           ┌────────────────┴────────────────────────┐
   logical drivers   │  bme280        ssd1306      your_device │  ← portable across MCUs
                     └────────────────┬────────────────────────┘
   Layer 2           ┌────────────────┴────────────────────────┐
   bus abstraction   │  spi-bus       i2c-bus      uart-bus    │
                     └────────────────┬────────────────────────┘
   Layer 1           ┌────────────────┴────────────────────────┐
   physical drivers  │  esp32-spi     esp32-i2c    esp32-uart  │  ← portable across devices
                     └────────────────┬────────────────────────┘
                     ┌────────────────┴────────────────────────┐
   kernel            │  scheduler · IPC · timers · IRQ router  │
                     └────────────────┬────────────────────────┘
                     ┌────────────────┴────────────────────────┐
   soc               │  soc/esp32   (pin mux, peripheral map)  │
                     └────────────────┬────────────────────────┘
                     ┌────────────────┴────────────────────────┐
   arch              │  arch/xtensa (trap, tick, context)      │
                     └─────────────────────────────────────────┘
```

The boundary between Layer 3 and Layer 1 is the whole point. A `bme280` driver
depends only on `api` — it cannot name an ESP32 register even if it wants
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
make flash
```

That builds `apps/demo`, flashes over USB serial, and opens a monitor.

The kernel is a library; the thing you flash is an **application** from
[`apps/`](apps/). Pick a different one, a different board, or a different amount
of logging:

```bash
make apps                                  # what's available
make flash APP=hello                       # the minimal one-task template
make flash APP=demo BOARD=board-m5-atom    # M5Stack Atom (ESP32-PICO-D4)
```

`DEBUG` defaults to `debug-level-1`, which is what you want while developing.
`debug-level-0` compiles logging out entirely — the tasks still run, they just
print nothing, so leave it alone until you are shipping.

Board features: `board-esp32-wrover` (default), `board-esp32-devkitc`,
`board-m5-atom`. Enabling two is a compile error, not a warning — a silently
wrong pin map presents as the board being broken.

Writing your own application is copying `apps/hello/` and adding a line to the
workspace `Cargo.toml`. See [`apps/README.md`](apps/README.md).

<details>
<summary><b>If flashing fails</b></summary>

**`Error while connecting to device`, right after `Using flash stub`** — the
baud switch failed. This is the common one. Flashing defaults to 115200 for
that reason; if you raised it, put it back:

```bash
make flash FLASH_BAUD=115200
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

Real output from an ESP32-PICO, trimmed:

```
[FLINT] FlintMain reached (_start -> Rust OK)
[FLINT] VECBASE=0x40080000 _vector_table_start=0x40080000 MATCH (vector table installed)
[FLINT] PS=0x0006000f WOE=1 (window overflow/underflow enabled)
[FLINT] SP=0x3ffb36e0 task_stack_pool=[0x3ffc0000, 0x3ffd8000)
[FLINT] startup::init done
[FLINT] cpu_hz=80000000 (measured: CCOUNT timed against RTC slow clock)
[FLINT] tick period=80000 CCOUNT ticks
[    0][task:0] INFO  [kernel] Flint RTOS boot complete, entering idle
[FLINT] interrupts unmasked, entering idle
[    2][task:1] INFO  [sensor] prio=Normal(1) n=1
[    5][task:2] INFO  [consumer] prio=Normal(5) n=1
[  505][task:1] INFO  [sensor] prio=Normal(1) n=2
[ 1005][task:1] INFO  [sensor] prio=Normal(1) n=3
[ 1010][task:2] INFO  [consumer] prio=Normal(5) n=2
[ 3010][task:3] INFO  [housekeep] prio=Background(1) n=1
```

The bracketed number is the tick, so the periods are readable straight off:
sensor every 500 ms, consumer every 1000 ms, housekeep every 3000 ms.

Each banner line proves the step before it, so the **last line you see tells you
where it died**:

| Last line | What it means |
|---|---|
| *(nothing)* | Dead before Rust, or the console baud is wrong |
| `FlintMain reached` | The vector table didn't install — check `VECBASE` |
| `VECBASE ... MISMATCH` | Traps will go to ROM; nothing will schedule |
| `WOE=0` | Register windows off; every windowed call is unreliable |
| `cpu_hz=... (ASSUMED)` | Clock measurement failed; every timeout is scaled by a guess |
| `entering idle`, then silence | It died in its first-ever interrupt — trap entry or `_flint_trap` |
| `DBL <cause> <epc1> <depc> <vaddr>` | Double exception. Those four words locate it exactly — include them in the report |

If tasks appear but the timing is off by a constant factor, that's the CPU clock
([#6](https://github.com/cooljackal/flintos/issues/6)). A genuine stack overflow
reports itself by name.

Set `TRAP_DIAGNOSTICS` in [`kernel/src/switch.rs`](kernel/src/switch.rs) to
`true` for a per-tick heartbeat reporting the running task, the interrupted PC
and `WINDOWSTART`. That is what a silent kernel needs, because a kernel that
never schedules and one whose timer never ticks are otherwise indistinguishable.

> **If it doesn't work, that is the single most useful thing you can report.**
> Open an issue with your board model and the raw serial output, garbled or not.

---

## Writing a task

A whole application, `apps/hello/src/main.rs`:

```rust
#![no_std]
#![no_main]

use api::task;
use hal::types::Priority;

kernel::flint_app!(main, abi = 1);

fn main() {
    task::spawn("blink", blink, Priority::Normal(1), 4096);
}

fn blink() {
    loop {
        api::log_info!("tick at {} ms", api::timer::now_ms());
        task::sleep_ms(500);
    }
}
```

`main` runs once, after the kernel is up but before interrupts are unmasked.
Spawn your tasks and return.

Priorities are banded — `Critical(0..15)`, `Normal(0..15)`, `Background(0..15)` —
so you can slot a task in without renumbering everything else.

### Talking between tasks

```rust
use api::queue::{self, Queue};

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
            api::log_info!("got {}", v);
        }
    }
}
```

Queues are typed, bounded, and `send_isr()` is safe to call from an interrupt
handler.

### Sharing state

```rust
use api::mutex::{self, Mutex};

static COUNTER: Mutex<u32> = Mutex::new(0);

fn bump() {
    let mut guard = mutex::lock(&COUNTER);
    *guard += 1;
}   // released on drop, with priority inheritance unwound
```

---

## Supported hardware

| Board | SoC | Status | Pinout |
|---|---|---|---|
| M5Stack Atom (Lite/Matrix) | ESP32-PICO-D4 (Xtensa LX6) | ✅ Boots, schedules, preempts | [wiki](https://github.com/cooljackal/flintos/wiki/Board-M5Stack-Atom) |
| ESP32-WROVER | ESP32 | Manifest present, untested | [wiki](https://github.com/cooljackal/flintos/wiki/Board-ESP32-WROVER) |
| ESP32-DevKitC / WROOM-32 | ESP32 | Should work — same manifest, untested | [wiki](https://github.com/cooljackal/flintos/wiki/Board-ESP32-DevKitC) |
| STM32F3 / F4 (Cortex-M) | ARM32 | Planned — needs a whole `arch-cortex-m` | — |

Adding a board is one file — see
[Adding a Board](https://github.com/cooljackal/flintos/wiki/Adding-a-Board).

The wiki carries the full [ESP32 pin table](https://github.com/cooljackal/flintos/wiki/SoC-ESP32):
which pins are safe, which are strapping, which are input-only, what's reserved
for flash and PSRAM, plus the peripheral map and the GPIO-matrix signal indices.
The point is not having to go looking somewhere else.

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
apps/                  applications — the binaries you actually flash
hal/                   traits + types every layer depends on (depends on nothing)
api/                   the API your application code uses
arch/xtensa/           CPU — boot, vectors, context switch, tick
soc/esp32/             chip — peripheral map, IO_MUX, GPIO matrix, clock gating
kernel/                scheduler, IPC, timers, IRQ routing, debug — a library
board/                 PCB — which pin is wired to what
drivers/physical/      Layer 1 — MCU register drivers
drivers/bus/           Layer 2 — transport abstractions
drivers/logical/       Layer 3 — device drivers, MCU-agnostic
tools/build/           build-script helper that gives an app the linker script
tools/size/            `make size` — where the image's bytes went, per region
tools/check-layers.sh  enforces the Layer 3 → Layer 1 boundary
doc/wiki/              the wiki's source; CI publishes it on merge
doc/internal/          superseded planning documents, kept as history
```

**arch / SoC / board are three separate tiers, deliberately.** The CPU core, the
chip, and the circuit board are three different things that vary independently:
ESP32 and ESP32-S3 share neither peripheral map nor core, while a WROVER and an
M5Stack Atom share both and differ only in wiring. Keeping them apart is what
makes adding a board one small file instead of a `cfg` tree.

The sharpest case is pin routing, which has no portable implementation: the
ESP32 has a GPIO matrix that reaches almost any pad, STM32 has fixed
alternate-function lists per pad, NXP has IOMUXC. Drivers call
`PinMux::route(signal, pin, config)` and each SoC crate implements it in its own
idiom, returning an error where the silicon genuinely cannot comply.

~11,000 lines of Rust and ~800 lines of Xtensa assembly.

## Development

```bash
make               # list every target, with what it does
make check         # host-side compile check
make lint          # clippy, warnings denied
make check-layers  # three-layer boundary enforcement
make check-names   # package naming and layout convention
make apps          # list applications
make build         # Xtensa build of APP (needs the esp toolchain)
make size          # where the image's bytes went, per memory region
```

### Tests

Two tiers, because they catch different things.

```bash
make test-host                                   # every change, no hardware
make test-target BOARD=board-m5-atom PORT=COM5   # when you have a board
```

**`make test-host`** runs the unit tests for every crate, kernel included — 160
of them, and CI runs them on every push. The kernel reaches the machine only
through `kernel::arch`, which substitutes stand-ins on a host, so its logic is
testable without an ESP32 at all.

Those stand-ins are honest about what they are. `cs_with` masks nothing on a
host, because there is nothing to mask — so races, priority inversion and
register-window corruption are not merely untested there, they are unreachable.
A green host run means the logic is consistent, not that the kernel works.

**`make test-target`** is the other half: five checks that run on real silicon,
each picked because a host cannot fail it — a trap corrupting the interrupted
task's register windows, a window spilled past the physical register file, and
whether a critical section genuinely masks the timer. It flashes the board,
reads the serial output and exits non-zero if anything failed, so it is usable
from a script rather than only by eye.

Set `PORT` when more than one serial device is attached, or espflash will stop
to ask which one and the harness has no terminal to answer with. `BOARD` must
match your hardware — the default is `board-esp32-wrover`, so an ESP32-PICO
board such as the M5Stack Atom needs `BOARD=board-m5-atom`.

`make test-harness` checks the parsing logic that decides whether a board
passed, without needing a board. It runs in CI, because a harness that reports
green when the serial line dropped half the output is worse than no harness.

`make build` prints the size report itself. It reports per *region*, not per
section, because a total says nothing useful: an ESP32 image is scattered across
memories with wildly different budgets, and the one that runs out is IRAM or
DRAM long before flash.

```
  Flint image: demo
+----------------+------------+------------+----------------------+--------+
| REGION         |       USED |   CAPACITY | USAGE                |   FULL |
+----------------+------------+------------+----------------------+--------+
| drom_seg       |   18.9 KiB |    4.0 MiB | #................... |   0.5% |
| dram_seg       |   16.5 KiB |   64.0 KiB | #####............... |  25.8% |
| task_stacks *  |   96.0 KiB |   96.0 KiB | #################### | 100.0% |
| vectors_seg    |      963 B |    1.0 KiB | ###################. |  94.0% |
| iram_seg       |      678 B |  127.0 KiB | #................... |   0.5% |
| irom_seg       |   28.4 KiB |    3.2 MiB | #................... |   0.9% |
+----------------+------------+------------+----------------------+--------+
| flash total    |   56.6 KiB |    3.9 MiB | #................... |   1.4% |
+----------------+------------+------------+----------------------+--------+
```

Region bounds come from the linker script, so the report cannot drift from the
map the image was linked against.

Two things the table marks rather than hides. `task_stacks` at 100% is a
reservation, not usage — nothing in it is stored in the image, and per-task
high-water marks come from `debug::stack` at runtime. And espflash will report a
larger number than `flash total`: the ESP32 maps flash in 64 KiB pages, so the
image builder pads to land the mapped segments on a page boundary. That padding
is real flash but it is not anyone's code.

Debug features are additive and compile out entirely:
`debug-level-0` (silent) through `debug-level-3` (full trace).

## Roadmap

- **Done** — ESP32 bring-up: trap handler, register-window spill, context
  switch, preemption and tick all proven on silicon. Applications split out of
  the kernel. arch/SoC/board split, with GPIO-matrix pin routing. Wiki docs.
- **Now** — getting the kernel's own tests to run somewhere
  ([#17](https://github.com/cooljackal/flintos/issues/17)).
- **Next** — I²C against a real device; driver register audit against the TRM.
- **Later** — Layer-1 drivers as isolated tasks with one-IPC-hop request/reply;
  `nsh` shell; Cortex-M port for STM32F3/F4.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Commits need a DCO sign-off (`git commit -s`).
The most valuable contribution today is a bug report from real hardware.

## License

[Apache-2.0](LICENSE). Patent grant included — see section 3.
