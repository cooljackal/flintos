<!-- SPDX-License-Identifier: Apache-2.0 -->

# Plan: make apps, boards and drivers pleasant to write

Status: proposed, 2026-08-22. Produced from a reviewed survey of every
`apps/*/src/main.rs`, `board/src`, `drivers/{physical,bus,logical}`, `hal` and
`api`; each finding was checked against the tree by a second reviewer whose
job was to refute it. What survived is below.

## The bottom line

App code is dense because **apps are doing the kernel's, the board's and the
driver's jobs**, not because Rust is dense. `apps/hello` and `apps/demo` read
fine. `imu`, `spitxrx`, `uartecho`, `blink`, `pwm` and `spidma` do not, and
every one of them is hard for the same five reasons:

| Symptom in the app | Root cause | Fix (all vanilla Rust) |
|---|---|---|
| `static mut X: Option<T>` + `addr_of!` + `unsafe fn bring_up() -> Option<()>` | Layer-2 buses demand `&'static dyn PhysicalBus`; nothing in `api` can pin a value into a static | `api::sync::Once<T>`, by-value `I2cBus<P>`/`SpiBus<P>`, board owns the instance |
| `soc_esp32::addr::I2C0_BASE`, `const SPI2: u8 = 2`, `CPU_INT = 13` in apps | Layer-1 constructors are `unsafe fn new(base: u32)`; the board's `TARGET_BUSES` table has no consumer | Controller enums in `soc_esp32`, `Driver::open(&board::IMU_I2C)`, `interrupt::connect` picks its own slot |
| `hal::bus::PhysicalBus::init(&mut phys, &cfg)` | Trait not in scope; no prelude | `api::prelude` |
| `.map_err(\|_\| "tx chain")?` ×21, `.ok()?`, `.and_then` chains | 16 unrelated error enums, zero `From` impls | `hal::Error` + `api::Result`, so `?` stands alone |
| `unsafe { ch.set_percent(50) }` | 84 `pub unsafe fn` on ordinary driver methods | Constructor carries the proof; methods become safe |

Target: an app imports `api::prelude::*` and `board`, and contains **no**
`unsafe`, `static mut`, `'static dyn`, `soc_*::`, `kernel::` or `map_err`.
If it needs one, that is a missing API and gets an issue, not a workaround.

The `imu` app after the plan:

```rust
#![no_std]
#![no_main]

use api::prelude::*;
use mpu6886::Mpu6886;

kernel::flint_app!(main, abi = 1);

fn main() {
    Task::new("imu", imu).spawn().expect("spawn");
}

fn imu() -> ! {
    let dev = Mpu6886::new(board::imu_bus().unwrap_or_else(|e| fail("imu bus", e)));
    dev.bring_up().unwrap_or_else(|e| fail("imu bring-up", e));
    loop {
        sleep_ms(500);
        match (dev.accel(), dev.gyro()) {
            (Ok(a), Ok(g)) => log_info!("accel {a} | gyro {g}"),
            _ => log_error!("read failed"),
        }
    }
}

fn fail(what: &str, e: Error) -> ! {
    log_error!("{what}: {e}");
    task::exit()
}
```

Same hardware, same drivers underneath. Every layer the app used to touch is
still reachable (`board::imu_bus()` hands back an `&I2cBus<Esp32I2c>`, and
`Esp32I2c::new(base)` stays `pub unsafe` for the self-tests), so dropping to
register level costs one `use`, not a different framework.

## What is explicitly NOT in this plan

- **No proc macros, attribute macros or new `macro_rules!`.** `#[task]`
  hides two lines and adds a layer to learn. `#![no_std]`/`#![no_main]` are
  inner attributes and cannot be emitted by a macro at all, so the entry
  boilerplate floor is three lines; a `make new-app NAME=x` copy handles that.
- **No `static_cell`, `embassy`, `embedded-hal` dependencies.** The shapes are
  borrowed (they are the right shapes), the code is ~30 lines each in-tree.
- **Numeric prefixes on app directories.** See §Apps layout.

---

## Phase 0 — bugs found in passing (fix first, independently)

These are correctness issues the survey tripped over. None is ergonomic; all
are cheap, and two of them would otherwise be silently preserved by the
refactor.

| # | Bug | Where |
|---|---|---|
| 0.1 | `BusHandle::read_reg` / `transfer(&[reg], buf)` is **wrong over SPI**: `spi-bus` forwards one `Op::exchange` and `esp32-spi` clocks `min(tx.len(), rx.len())` bytes, so a 1-byte tx with an N-byte rx reads one byte. Works on I²C only. | `drivers/bus/spi-bus/src/lib.rs`, `hal/src/bus.rs` |
| 0.2 | `SpiBus` silently truncates write-only / read-only ops at 64 bytes (`tx.len().min(MAX_TRANSFER)`), contradicting the `Bus` doc's "a caller may send any length". | `drivers/bus/spi-bus/src/lib.rs:43-55` |
| 0.3 | `blink` takes `&mut STREAM` from the task (`:294`) **and** from the RMT ISR (`:364`). Two live `&mut` to one static is UB regardless of timing. | `apps/blink/src/main.rs` |
| 0.4 | CI's Xtensa app loop has been red-but-green: `make check-all` runs `cargo check --target thumbv6m-none-eabi -p arm-selftest` and CI never installs that target (`E0463 can't find crate for core`), and the job is `continue-on-error: true`. The `for app in apps/*/` loop also feeds `arm-selftest` a `board-esp32-devkitc` feature it does not declare. | `.github/workflows/ci.yml:185,252`, `Makefile:451` |
| 0.5 | `HAS_WIFI = true` on the Wio RP2040 Mini: its ESP8285 is a UART module, not a PHY the SoC drives; `kernel/src/radio.rs:76` would try to bring it up. | `board/src/wio_rp2040_mini.rs:16` |

---

## Phase 1 — the floor: prelude, error type, `Once`, `park`

Small, mechanical, no behaviour change. Lands in one PR. Everything after
depends on it.

### 1.1 `hal::Error` and `api::Result`

One error type an app can `?` into. Lives in `hal` (SoC-free; `api` may not
name `kernel`, so it cannot live there and wrap kernel types).

```rust
// hal/src/error.rs
pub enum Error {
    Bus(BusError), Wifi(WifiError), PinMux(RouteError), Dma(DmaError),
    Interrupt(ConnectError), Timer(TimerError),
    NotInitialised, Unsupported, WrongDevice { expected: u8, found: u8 },
    Other(&'static str),
}
pub type Result<T> = core::result::Result<T, Error>;
impl From<BusError> for Error { .. }   // one per variant
impl Display for Error { .. }
```

- `ConnectError` and `DmaError` move from `kernel` to `hal` (plain enums, no
  kernel state) so kernel and api name the same type.
- Each physical driver adds `impl From<ItsError> for hal::Error` — they already
  depend on `hal`.
- Every `.map_err(|_| "..")?` in the tree becomes `?`. (21 sites in apps.)
- `WifiError::Backend(i32)` already exists and already has `Display`; reuse.

### 1.2 `api::sync::Once<T>`

The `static_cell` shape, ~30 lines: `UnsafeCell<MaybeUninit<T>>` +
**`portable_atomic::AtomicU8`** state (not `core::sync::atomic` — the RP2040
Cortex-M0+ has no CAS and `api` already depends on `portable-atomic` for that
reason). `init(&'static self, T) -> &'static T` (panics on second init),
`get() -> Option<&'static T>`, `get_or_try_init(f) -> Result<&'static T, E>`.
`unsafe impl<T: Sync> Sync`.

This replaces every `static mut X: Option<T>` + `addr_of!` in apps and in
`kernel/src/selftest_spi.rs:67`, `selftest_spi_slave.rs:128`.

For anything shared between a task and an ISR (blink's stream, the DMA
completion in spidma) pair it with a `hal::critical_section`-guarded cell so
both sides go through `with(|s| ..)`. That is also the fix for bug 0.3.

### 1.3 `api::prelude`

Plain `pub use`, nothing else:

```rust
pub use crate::{log_info, log_warn, log_error, task, timer};
pub use crate::task::{Task, spawn, sleep_ms, exit};
pub use crate::sync::Once;
pub use crate::{Error, Result, Priority, TaskId};
pub use hal::bus::{Bus, BusHandle, BusConfig, BusSpeed, SpiMode, PhysicalTransfer};
pub use hal::pinmux::{PinMux, PinConfig, Signal};
pub use hal::stream::ByteStream;
```

- `hal::pinmux` is the one surface `api` does not re-export today; that is the
  only reason every app `Cargo.toml` carries a `hal` line. Drop it from all 14.
- The board cannot be re-exported from `api` (`api` does not depend on
  `kernel`). Apps depend on the `board` crate directly and write
  `use board::active as board;` — the Zephyr shape. Drops
  `kernel::board::active` reach-through from five apps.

### 1.4 `task::exit() -> !`, `Task` builder, `#[must_use]`

- `pub fn exit() -> !` via a new `_flint_sys_task_exit` stub calling the
  existing `flint_task_exit`. Replaces the five hand-rolled `park()`/`idle()`
  loops. Failure paths become `let Some(x) = .. else { log_error!(..); task::exit() }`.
- Builder with defaults, so the 7 call sites that spell
  `Priority::Normal(1), 4096` stop: `Task::new(name, entry).spawn()`; override
  with `.priority(..)`, `.stack(..)`, `.on_core(..)`. Returns the existing
  `Option<TaskId>` marked `#[must_use]` — no new error enum until the syscall
  can distinguish pool-full from bad-core (that is an ABI bump). Fix the 13
  sites that drop the result in the same commit.
- `task::wait_until(cond, timeout_ms) -> bool` over `sleep_ms` +
  `timer::now_ms` (which already exists; three apps re-derive it with
  `kernel::clock::now_us`).

### 1.5 Log tags

161 of 218 app log lines hand-write `[tag]`. The TCB already holds the task
name; stamp it into `LogEntry` (one pointer) and print `[imu]` from the
kernel. App lines drop the prefix. Check `tools/target-test.sh` greps first —
they key on the literal tags.

---

## Phase 2 — Layer 1: drivers stop leaking addresses and `unsafe`

### 2.1 Controller enums in `soc_esp32`

```rust
pub enum I2cCtrl { I2c0, I2c1 }   // likewise SpiCtrl, UartCtrl
impl I2cCtrl { pub const fn base(self) -> u32; pub const fn irq(self) -> u8; pub const fn clock(self) -> ClockBit; }
```

Drivers stop re-validating `addr::spi_instance(self.base).ok_or(..)?` on every
transfer (esp32-spi does it at `:423` and `:507`). Apps stop writing
`const SPI2: u8 = 2` next to `SPI2_BASE`. `hal::pinmux::Signal::SpiMosi(u8)`
keeps its `u8` (hal is SoC-free); the enum supplies it.

`Esp32Gpio` is a chip singleton: `soc_esp32::gpio() -> &'static Esp32Gpio`,
removing the `Esp32Gpio::new(addr::GPIO_BASE)` from pwm, blink and
`selftest_mcpwm`.

### 2.2 `open()` claims the controller once; `new()` stays `unsafe` for tests

```rust
impl Esp32I2c {
    /// Safe: claims I2C0/I2C1 once per boot; a second `open` returns `Error::Bus(Busy)`.
    pub fn open(port: &I2cPort) -> hal::Result<Self>;
    /// Unchanged. For the kernel self-tests, which step through bring-up deliberately.
    pub unsafe fn new(base: u32) -> Self;
}
```

- Claim is a crate-private `[portable_atomic::AtomicBool; N]` indexed by the
  enum. This is the `svd2rust::Peripherals::take()` pattern; it discharges the
  "not concurrently owned" invariant that is the **whole** `# Safety` text on
  `new` today, which is what lets the constructor be safe.
- `I2cPort`/`SpiPort`/`UartPort` are `Copy` structs in `soc_esp32`
  (controller + config), no driver dependency, so the board crate can hold
  them as `const`.
- `open` does what `Esp32Spi::init` already does internally — clock gate, pad
  route, config — so LEDC and RMT get the same treatment: `Channel::on_pin(..)`
  and `Rmt::on_pin(..)` gate their own clock and route their own signal.
  `pwm` and `blink` then drop `soc_esp32` entirely. `PinConfig` needs an
  `input: bool` (read-back) field for pwm's self-measurement; `Esp32PinMux::route`
  already touches `FUN_IE`, one line.

### 2.3 Split `PhysicalBus` along its `&self`/`&mut self` seam

```rust
pub trait PhysicalTransfer: Send + Sync {
    fn exchange(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()>;
    fn set_speed(&self, _: BusSpeed) -> BusResult<()> { Err(Unsupported) }
}
pub trait PhysicalBus: PhysicalTransfer {  // construction-time only
    fn init(&mut self, cfg: &BusConfig) -> BusResult<()>;
}
```

- `impl<T: PhysicalTransfer + ?Sized> PhysicalTransfer for &T` is sound (no
  `&mut` methods). The reader's original blanket impl over `PhysicalBus` was
  rejected: it would have to lie about `init`.
- Delete `set_enabled` (required, never called, two drivers stub it) and
  `impl PhysicalBus for Esp32Gpio` (returns `Err` on every transfer).
- Rename `raw_transfer` → `exchange` (matches `Op::exchange`); esp32-spi's
  inherent 64-byte `transfer` → `fifo_exchange`, returning `Err` past 64 bytes
  instead of `debug_assert!`. Kills the three-`transfer`s ambiguity that forced
  UFCS.
- Replace the `BusConfig` enum's anonymous struct variants with `SpiConfig` /
  `I2cConfig` / `UartConfig` (`Default`, `const fn` helpers `uart_8n1`,
  `spi_mode0`). Four manifests and three apps re-type these literals today.

### 2.4 Safe methods where ownership is the proof

84 `pub unsafe fn` across `drivers/physical/esp32`. Audit each: if the
`# Safety` text amounts to "you must own the peripheral", the constructor
(§2.2) now proves that and the method becomes safe. Known: all of `esp32-ledc`
`Channel::{set_duty,set_percent,duty}`, `Timer::counter`; most of `esp32-rmt`.
Keep `unsafe` only where a wrong argument can corrupt memory (DMA descriptor
chains, cache-off flash).

### 2.5 DMA: the kernel picks the CPU interrupt, the driver owns the top half

- `kernel::interrupt::connect(source, handler) -> Result<CpuInt, ConnectError>`
  allocates from `intr_map::USABLE`; keep `connect_at` for callers that need a
  level. Deletes the magic `13` from spidma and blink.
- `Esp32Spi::exchange_async(&self, tx, rx, chain) -> Result<Transfer>` +
  `Transfer::await_done()`. The descriptor build, `dma_int_enable`, ack and
  `dma_broker::signal_complete` handoff move from `apps/spidma` into the driver;
  spidma shrinks to "fill, exchange, compare". The ISR reads the same
  `&'static` driver the task built (via `Once`), not a fresh `Esp32Spi::new`
  inside the trap handler.
- `api` stubs for what apps still reach into `kernel` for (50+ sites):
  `_flint_sys_now_us`, `_flint_sys_interrupt_connect`, `_flint_sys_dma_*`,
  `_flint_sys_current_core`, in the existing `extern "Rust"` pattern.

### 2.6 Driver-author conventions (write into `drivers/README.md`)

One constructor shape: `open(&Port) -> hal::Result<Self>`. One register
helper: use `soc_esp32::reg::{read,write,modify}` (7 drivers have a private
`fn reg(&self, off)` copy). Error type implements `From` into `hal::Error`.
No pad routing left to the caller. A `drivers/physical/esp32/_template/` that
does exactly this is worth more than the prose.

---

## Phase 3 — Layer 2/3: buses own their controller, handles borrow

### 3.1 By-value bus wrappers

```rust
pub struct I2cBus<P: PhysicalTransfer> { phys: P, .. }      // was &'static dyn PhysicalBus
pub struct SpiBus<P: PhysicalTransfer> { phys: P, .. }
```

Two callers (imu, spitxrx) plus the self-tests; no compatibility shim. One
static now holds the whole stack: `Once<I2cBus<Esp32I2c>>` instead of `PHYS` +
`BUS`.

### 3.2 Split I²C controller from I²C device

`I2cBus` bakes one slave address in, so a bus scan or a second device
bypasses Layer 2 and calls `raw_transfer` with the `tx[0]`-is-address
convention the app has to know. `I2cController<P>` (owns `P`, has `scan()`,
`device(addr) -> I2cDevice<'_>`) and `I2cDevice: Bus`. The `imu` scan
becomes `bus.scan()`.

### 3.3 `BusHandle<'a>` instead of `BusHandle<'static>`

Nothing in the handle needs `'static` (`Bus::transfer` is `&self`). Five
logical-driver test files fake `'static` with a transmute today; `ssd1306`'s
`cfg_attr(not(test), forbid(unsafe_code))` exists only for that. Add
`impl<'a, B: Bus> From<&'a B> for BusHandle<'a>` so
`Mpu6886::new(BusHandle::new(bus))` becomes `Mpu6886::new(bus)`.

Note: `BusHandle`'s layout is on the frozen ABI line — check
`api/src/lib.rs:31` and bump with a CHANGELOG Breaking entry.

### 3.4 Shared-bus locking — **blocker for board-owned handles**

`Esp32I2c`/`Esp32Spi` are auto-`Sync` and `exchange(&self)` takes no lock.
Today that is fine because each app owns its bus privately. Once the board
hands out `&'static I2cBus` to any task that asks, two tasks can interleave
transfers. `I2cController`/`SpiBus` take an `api::Mutex` around `exchange`
(the existing `api::mutex` is a syscall; ISR callers need the
critical-section path). Do this before §4, not after.

### 3.5 Logical drivers finish their own bring-up

`mpu6886` exposes `reset()`/`wake()/configure()` and tells the caller to sleep
10 ms between them. `bring_up()` does the sequence and the sleeps.
`BusHandle` grows `write_reg`/`read_regs` (with `kind() -> BusKind` so SPI
sets bit 7 and I²C does not — also the fix for bug 0.1) and loses the
always-`Ok(())` `select()`/`deselect()` that bme280 and ssd1306 bracket every
register op with.

---

## Phase 4 — Boards own devices

### 4.1 `board` gets a tier in `tools/check-layers.sh`

`{"hal","api"} | SOCS | physical | bus | LIBS`. The board crate may construct
drivers; it may not name `kernel`.

### 4.2 Named ports and device functions per board

```rust
// board/src/m5_atom_matrix.rs
pub const IMU_I2C: I2cPort = I2cPort { ctrl: I2cCtrl::I2c0, cfg: I2cConfig { sda: 25, scl: 21, speed: Standard100k } };
pub const IMU_ADDR: u8 = 0x68;

static IMU_BUS: Once<I2cController<Esp32I2c>> = Once::new();
/// The IMU's bus. First call opens I2C0 at 100 kHz on GPIO25/21; later calls return the same handle.
pub fn imu_bus() -> hal::Result<&'static I2cController<Esp32I2c>> {
    IMU_BUS.get_or_try_init(|| Ok(I2cController::new(Esp32I2c::open(&IMU_I2C)?)))
}
```

Same for `led()`, `console()`, `loopback_pads() -> Option<LoopbackPads>`.
The IMU attachment is half-declared today (pins + address, not which
controller); this closes it.

### 4.3 Console moves out of the kernel

`kernel/src/startup.rs` holds a `pub static mut CONSOLE_UART: Esp32Uart`
behind `#[cfg(feature = "soc-esp32")]` — the one board-owned device that
exists, in the wrong layer, not portable to RP2040. Every board exports
`console_init()` (called first thing by `startup::init` on the boot core)
and `console() -> Option<&'static dyn ByteStream>`. The kernel's
`esp32-uart` dependency goes away.

### 4.4 `Board` struct instead of loose consts

One `pub const BOARD: Board` per board, fields are `Option`s (`imu`,
`rgb_led`, `selftest: SelftestPads { scratch, aux, .. }`). Apps then
`compile_error!` on the **fact** (`board::BOARD.imu.is_none()`), not on a
feature name, and a board can be checked for completeness in one test.
SoC facts (`peripheral_window`, `max_gpio`) move to the `Soc` trait so
`board/src/lib.rs:192-206` stops keying them on board name.

### 4.5 Adding a board: one line, not six files

- Replace the O(n²) pairwise `compile_error!` table with
  `const _: () = assert!(SELECTED == 1, ..)` over `cfg!()` counts; the
  `active` arms drop their `not(..)` clauses.
- The 58 `board-*` feature-forwarding lines across `apps/*/Cargo.toml` go:
  the Makefile passes `--features kernel/board-x` directly (valid Cargo), so
  an app manifest has no board features at all.
- Workspace `members` become globs (`"apps/examples/*"`, `"apps/tests/*"`),
  so a new app is `cp -r examples/hello examples/mine` and nothing else.
  `apps/README.md` currently says "three files" then lists four.
- Clean `TARGET_*` tables: `TARGET_PERIPHERALS` duplicates uart0,
  `TARGET_SERVICES` names devfs/procfs that do not exist.

---

## Apps layout

Two directories, no numeric prefixes. Move only — separate commit from any
content change so the ergonomics diff stays reviewable; stage by explicit
path, another session may share the tree.

```
apps/
├── README.md            three sections: reading order, porting templates, verification
├── examples/            what a newcomer reads or copies
│   ├── hello            step 1 · any board
│   ├── demo             step 2 · any board · ALSO the on-target verification workload (say so in its header)
│   ├── blink            first peripheral · Atom
│   ├── pwm              Atom
│   ├── imu              I²C porting template · Atom Matrix
│   ├── spitxrx          SPI porting template · DevKitC
│   └── uartecho         UART porting template · DevKitC
└── tests/               PASS/FAIL contract; issue number in the header
    ├── smp              #20
    ├── spidma           #80
    ├── flashprobe       nvs handover
    ├── radioprobe       plan-radio 3.6 · blobs
    ├── wifiscan         plan-radio 5.2 · blobs
    ├── wificonnect      phase-3 acceptance · blobs, SSID env · PASS/FAIL ×5
    └── arm-selftest     RP2040 · machine-judged
```

**Why `wificonnect` is a test, not an example:** it prints PASS/FAIL more
times than spidma does, needs `EXTRA_FEATURES=blobs` plus credentials in the
environment, and cannot build in CI without `.blobs/`. Splitting it from
`wifiscan` across the boundary reads wrong. When the radio phase closes, a
trimmed `wifi` example can be cut from it.

**Why no `01-hello`:** Cargo forbids digit-leading package names, so the
directory would be `01-hello` and the package `hello`; `tools/check-names.sh`
rule 3 (leaf == name) then fails and needs an exception for the one directory
class meant to be simplest. `make apps`, `tools/upgrade.sh` and `ci.yml` all
derive the package from `basename`, and `make flash APP=01-hello` would
silently mean nothing. The order only holds for the first two apps anyway —
`blink`/`pwm` are board-locked and `imu`/`spitxrx`/`uartecho` are peer
templates, not steps 5–7. Carry the order in `README.md` and a `Next:` line
at the bottom of each example's header.

Every file the move touches (verified against the tree):

| File | Change |
|---|---|
| `Cargo.toml` members | two globs |
| `apps/*/Cargo.toml` | `../../` → `../../../` on every path dep (14 crates) |
| `apps/*/build.rs` | none — `tools/build` walks ancestors |
| `Makefile:139` HOST_EXCLUDE | derive: `$(notdir $(patsubst %/,%,$(wildcard apps/*/*/)))` — plain `$(notdir)` on a trailing slash yields empty |
| `Makefile:200-206, 372` | comment paths; `make apps` loops `apps/examples/*/ apps/tests/*/` with a group line |
| `Makefile:436,451,544` | none (package names) |
| `tools/upgrade.sh:41` | `apps/*/*/` — **fails quiet otherwise** ("No applications", exit 0) |
| `tools/target-test.sh` | none |
| `tools/check-names.sh` | none |
| `.github/workflows/ci.yml:252` | `apps/*/*/`; `arm-selftest) continue ;;` or a thumbv6m build; install the target before `check-all`; drop `continue-on-error` once green (bug 0.4) |
| `README.md:140,163` | paths (150-152 are `APP=` lines, leave) |
| `doc/nvs-flash-handover.md:5,107,115,191`, `doc/plan-radio.md:310,645,649,913,1024` | paths |
| `kernel/src/selftest_dport.rs:16`, `selftest_flash.rs:80`, `board/src/esp32_devkitc.rs:67`, `m5_atom_matrix.rs:23` | comment paths |
| `apps/imu:7`, `spitxrx:5,18,27`, `uartecho:5` | cross-reference paths in headers |
| Generated API reference | none — `make docs` excludes apps; rustdoc lays out by crate name |

---

## Order of work and what each phase buys

| Phase | PRs | Apps that become clean | Risk |
|---|---|---|---|
| 0 bugs | 3 small | — | none; pure fixes |
| Layout move | 1 | — | low; move-only commit |
| 1 floor | 1 | `hello`, `demo`, `smp` lose their last `kernel::` reach-throughs | ABI: none |
| 2 Layer 1 | 1 per driver (spi, i2c, uart, ledc, rmt, gpio) + 1 for DMA/interrupt | `pwm`, `blink`, `spidma` | self-tests are Layer-1's third client — they move with each driver PR |
| 3 Layer 2/3 | 2 (bus split + handle lifetime; locking) | `spitxrx`, `uartecho` | `BusHandle` is on the ABI line — one bump |
| 4 boards | 2 (device fns + console; `Board` struct + feature cleanup) | `imu`, `wificonnect` | console move touches boot order; do it with the RP2040 port's console in the same PR so it is proven on both |

Radio: `EspStation::bring_up(handler) -> WifiResult<EspStation>` does the
nvs/heap/init/mode/start sequence that wificonnect, wifiscan and radioprobe
each repeat with five `unsafe` blocks and `rc != 0` checks. Keep `unsafe fn
new` for radioprobe, which steps through the sequence on purpose. Can land
any time after Phase 1.

## The rule afterwards

An app may depend on `api`, `board`, `kernel` (for `flint_app!` only) and
logical drivers. `tools/check-layers.sh` gains an `apps` tier that enforces
it, and a grep in CI fails on `unsafe`, `static mut` or `addr_of` under
`apps/examples/`. `apps/tests/` is exempt — probing the machinery is its job.
