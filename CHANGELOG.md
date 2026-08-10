<!-- SPDX-License-Identifier: Apache-2.0 -->

# Changelog

What changed, and — where it matters — what you have to do about it.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). FlintOS
is pre-1.0 and moves fast, so **Breaking** is the section that earns this file
its place. Every entry there should say what to change, not only what changed.

Applications declare the ABI they were written against:

```rust
kernel::flint_app!(main, abi = 1);
```

A kernel that provides a different one refuses to build and points here.
`make upgrade` reports which of your applications an upgrade affects.

## [Unreleased]

### Added

- **The ESP32-DevKitC / WROOM-32 is verified hardware**, and is now the
  reference board for on-target work: 30 on-target tests pass (1 skipped),
  `flashprobe` completes a flash round trip with the second core running, and
  `smp` schedules on both. The default board is still the WROVER, which
  nobody has flashed — pass `BOARD=board-esp32-devkitc` for the tested path.

- **`make check-features`** — clippy for Xtensa across the feature
  combinations that gate real code. `make lint` runs on the host, so
  everything behind `target_os = "none"` was invisible to it: all of
  `arch-xtensa`, the trap handler, the self-test suite. Its first run found
  21 pre-existing errors in code no linter had ever read.

- **A third self-test outcome, `SKIP`.** A test that needs hardware this
  board has not got is neither a pass nor a failure, and dropping it silently
  shrinks the total with nothing saying why. The first user is the ADC test,
  which needs a pin the board holds high — `ADC_EXTERNAL_HIGH_GPIO` in the
  manifest, `Some(39)` on the Atoms and `None` elsewhere.

- **Persistent configuration in flash.** `kvstore` over the `nvs` partition:
  a value written one boot is read back the next. `kernel::nvs::FlashStorage`
  is the joint; the driver underneath drives SPI1 directly, because the ROM's
  flash routines are the two Espressif themselves replace.

- **A heap, confined to the radio.** `lib/heap` is a free-list allocator and
  `kernel::heap` gives it the memory reclaimed above the static map — about
  126 KiB. It is deliberately **not** a `#[global_allocator]`: registering one
  would put `alloc::` in reach of every crate and end the no-allocation
  property that makes latency bounded. Nothing in the kernel calls it.

- **Runtime-created kernel objects**, for the same reason: `kernel::dynobj`
  has byte-copying queues, counting semaphores, recursive mutexes, event
  groups and heap-backed tasks that can be deleted. A second object model
  beside the static one, not a replacement — `Queue<T, N>` and friends are
  unchanged.

- **Radio configuration as features.** `radio-wifi`, `radio-ble` and
  `radio-bt-classic`, selected the same way as boards and debug levels:

  ```bash
  make flash APP=demo BOARD=board-m5-atom-matrix EXTRA_FEATURES=radio-wifi,radio-ble
  ```

  Boards declare `HAS_WIFI` and `HAS_BT`, and asking for a radio the board has
  not got is a build error rather than a runtime disappointment.

- **`make blobs`** fetches Espressif's Wi-Fi and BLE archives (Apache-2.0,
  ~4.3 MB), pinned to the revisions esp-idf references. They are fetched
  rather than committed. **`make blob-symbols`** reports what they still need
  from us.

### Changed

- **The memory map is generated.** `arch/xtensa/flint32.ld` still contains the
  numbers, and `tools/build::link()` rewrites them when a radio feature needs
  the map moved. **A build without Bluetooth is byte-for-byte what it was** —
  there is a test pinning the literal addresses. With `radio-ble` the map
  shifts up 56 KiB to leave the controller the bottom of DRAM, and the
  per-task stack pool shrinks from 96 KiB to 80 KiB to pay for it.

- **DMA reaches all of internal DRAM**, not just SRAM2. `soc_esp32::dma`'s
  reachability check stopped at `0x3FFDFFFF` and had been rejecting valid
  buffers in SRAM1 since the DMA work landed. esp-idf's `SOC_DMA_HIGH` is
  `0x40000000`. Nothing you wrote was wrong; some things you could not do now
  work.

### Fixed

- **Every blocking queue send or receive panicked.** `queue::deadline_for`
  read the tick through `scheduler::with` while `block_send`/`block_recv` were
  already holding that lock. Reentrancy on a `Spinlock` is a panic, not a wait,
  so any `recv` on an empty queue or `send` on a full one died on the spot —
  from the moment the scheduler became a spinlock. Nothing caught it: the
  queue's tests exercise `try_send`/`try_recv`, which never block, and the
  on-target suite runs in boot context where blocking is refused anyway.
  The tick is now read before the lock is taken.

- **`dma_broker::submit` and `await_transfer` no longer pretend.** They
  returned `NotImplemented` and always had. `submit` is replaced by `begin`,
  which mints a transfer id and checks buffer ownership; `await_transfer`
  takes a timeout and actually waits. Programming the engine belongs to the
  driver that owns the peripheral — there is no portable engine for the kernel
  to program, which is why the old signature could never have been implemented.

- **The DMA pool was zero bytes on hardware.** `.dma_pool` in the linker
  script contained only `*(.dma_buffer)`, and nothing in the tree emits that
  section — the region is handed out at runtime, not declared by a static. So
  `_dma_pool_start` and `_dma_pool_end` were the same address and every
  `dma_broker::alloc` failed with `PoolExhausted`. On the target only: the
  broker's host tests state a pool size rather than deriving one from the
  linker symbols, so they had always passed. The section now claims its whole
  region, and a link-time `ASSERT` fails the build if it is ever empty again.

- **SPI was never full duplex.** `transfer(tx, rx)` promises a simultaneous
  exchange, which is what every SPI device expects, but `SPI_DOUTDIN` was
  never set — so the MOSI and MISO phases ran one after the other and the
  read clocked in a line nothing was driving. Loopback returned zeros; a real
  device would have returned garbage.

- **GPIO16 and GPIO17 are not free on the Atom.** `board` advertised them as
  `PSRAM_FREE_GPIOS`, reasoning that the ESP32-PICO-D4 has no external PSRAM.
  It has no external *flash* either — the flash is inside the SiP, and those
  two pins are part of reaching it. Routing a peripheral onto GPIO16 kills the
  running image mid-instruction, with no fault and no reset. Renamed to
  `RESERVED_GPIOS` and documented.

- **DPORT was accessed unsafely from both cores.** Two independent hazards, and
  neither was reachable until the scheduler started running on core 1.

  The ESP32 has a silicon erratum: a DPORT read taken while the other CPU
  accesses APB can return the APB value. Nothing faults — the caller just gets
  the wrong number. `soc_esp32::dport::read` now applies Espressif's
  workaround (an APB pre-read immediately before the DPORT load, interrupts
  masked, the two loads adjacent), and every DPORT access in the tree goes
  through it. Writes are a plain store, which esp-idf's own header documents as
  needing no protection.

  Separately, `enable`/`disable` read-modify-write two shared registers, so two
  cores gating different peripherals could lose each other's bits. Those now
  hold a lock across the whole sequence, both registers under one acquisition.

- **`make test-target` failed a passing board.** The judge shelled out to `sed`
  to read the summary counts, and under `make` the PATH picks up a different
  toolchain's `sed` that did not match the pattern. It parses with a bash
  builtin now, which has no PATH lookup to get wrong.

- **The ESP32 I²C controller was never correctly initialised.** `init` wrote
  `I2C_FIFO_CONF` with bit 13 set, commented as an interrupt enable; bit 13 is
  `I2C_TX_FIFO_RST`, so the transmit FIFO was pinned in reset and no byte could
  leave the controller. It also never set `SCL_FORCE_OUT`/`SDA_FORCE_OUT`, left
  every START/STOP shaping register at its reset value, and set the bus timeout
  to 0 — the *shortest* timeout, not none.
- **NAKs were invisible.** Command words never set `ack_check_en`, so a NAKed
  address completed like a real one and a bus scan reported all 112 addresses
  present.
- **A failed transaction wedged the controller.** The ESP32 does not unwind a
  NAK: it stops without issuing STOP and the next transaction inherits the
  state. Failures now cycle the peripheral through DPORT and reprogram, as
  esp-idf's `i2c_hw_fsm_reset` does.

- **The ESP32 I²C driver never returned the bytes it read.** `read` programmed
  the READ commands, waited for completion and left the data in the RX FIFO,
  returning `Ok(())`. Every I²C read this driver has ever done returned
  nothing — which is consistent with I²C never having been confirmed against a
  real device. It now takes a buffer and drains the FIFO.
- **The I²C address was shifted twice.** The bus layer pre-shifted in `write`
  and not in `transfer`, while the physical driver shifted again, so `0x76`
  reached the wire as `0xD8` and nothing would ACK. The convention — `tx[0]` is
  the 7-bit address, unshifted — is now written down on
  `hal::PhysicalBus::raw_transfer`, where both sides can see it.
- **A write-only or read-only I²C transfer did nothing and returned `Ok`.**
  `raw_transfer` acted only when both `tx` and `rx` were non-empty.
- **`I2cBus::read` addressed the general-call address**, sending a zeroed `tx`
  rather than the device address.

### Breaking

- **Layer-1 drivers are grouped by SoC.** `drivers/physical/esp32-uart/` is now
  `drivers/physical/esp32/uart/`, and the same for the other seven. Package
  names are unchanged — `esp32-uart` is still `esp32-uart` — so only a `path =`
  in an out-of-tree application needs editing:

  ```toml
  esp32-gpio = { path = "../../drivers/physical/esp32/gpio" }
  # was:       path = "../../drivers/physical/esp32-gpio"
  ```

  The SoC is the unit of portability at Layer 1: every crate under `esp32/`
  depends on `soc-esp32` and none of them run anywhere else. A flat directory
  sorted `esp32-rmt` next to `esp32-rng` while a second chip's SPI driver would
  land nowhere near this one's — grouping by peripheral name rather than by the
  thing that decides whether two crates share anything.

- **RMT, the watchdogs and the RNG moved out of `soc-esp32` into their own
  physical drivers.** A peripheral is something you write a driver for; the SoC
  crate holds what every driver needs underneath it.

  ```rust
  use esp32_rmt::{Entry, Rmt};   // was: soc_esp32::rmt::{Entry, Rmt}
  ```

  ```toml
  esp32-rmt = { path = "../../drivers/physical/esp32/rmt" }
  ```

  `kernel::rng` and `kernel::watchdog` are unchanged — the kernel re-exports
  them from the new crates.

- **`board-m5-atom` split into `board-m5-atom-lite` and
  `board-m5-atom-matrix`.** The Atom Lite has one LED and the Atom Matrix has a
  5×5 panel on the same pin, and one feature could not tell them apart — an
  application told only `RGB_LED_GPIO` drove the first LED of a panel and
  looked correct while 24 stayed dark.

  ```sh
  make flash APP=demo BOARD=board-m5-atom-matrix   # was: BOARD=board-m5-atom
  ```

  ```toml
  # in your application's Cargo.toml
  board-m5-atom-matrix = ["kernel/board-m5-atom-matrix"]
  ```

  The old name is still accepted and fails with a message naming the two
  replacements, rather than leaving cargo to say "does not contain this
  feature".

- **Applications must declare an ABI version.** `flint_app!(main)` no longer
  compiles.

  ```rust
  kernel::flint_app!(main, abi = 1);   // was: kernel::flint_app!(main);
  ```

  Without a declaration there is nothing to check, and an unversioned
  application is exactly the one that breaks silently on a kernel upgrade.

- **Every package lost its `flint-` prefix.** `flint-hal` → `hal`, `flint-api`
  → `api`, `flint-kernel` → `kernel`, `flint-arch-xtensa` → `arch-xtensa`,
  `flint-soc-esp32` → `soc-esp32`, `flint-board` → `board`.

  Update the `path` dependencies and the `use` statements in your application:

  ```toml
  kernel = { path = "../../kernel", default-features = false }
  api    = { path = "../../api" }
  hal    = { path = "../../hal" }
  ```

  ```rust
  use api::task;              // was: use flint_api::task;
  use hal::types::Priority;   // was: use flint_hal::types::Priority;
  ```

- **Directories were renamed to match**: `flint-hal/` → `hal/`,
  `arch/flint-arch-xtensa/` → `arch/xtensa/`, `soc/flint-soc-esp32/` →
  `soc/esp32/`, `drivers/**/esp32_uart/` → `drivers/**/esp32-uart/`, and
  `flint-build/` → `tools/build/`. Only affects you if you referenced a path
  directly.

- **The `phase0-tests` feature is now `self-test`.**

### Added

- **Watchdogs**, off unless an application opts in with
  `kernel::watchdog::arm()`. Two of them: the RTC watchdog is fed from the timer
  interrupt and catches a kernel that has stopped servicing it, and a
  timer-group watchdog is fed from the idle task and catches a task that never
  yields. Neither catches the other's failure — a spinning task keeps the tick
  alive, so only the idle-fed one notices.
- **`apps/blink`**, which drives the M5Stack Atom's onboard LED. It is also
  the on-hardware test for the RMT register map — no host test can tell you
  whether a register is where you think it is.
- **`tools/check-layers.sh` polices every tier**, not three of them: `hal`
  depends on nothing, `arch/*` and `soc/*` on `hal`, `drivers/physical/*` on
  `hal` and `soc/*`, bus and logical drivers on `api` and `lib/*`, `lib/*` on
  each other. 17 crates checked, up from 7 — `drivers/physical/` was entirely
  unchecked, so the layering could be inverted with CI green.
- **`#![forbid(unsafe_code)]` in every logical driver.** The dependency check
  cannot stop a driver writing to a register, because raw MMIO needs no
  dependency. This is the lint that makes the guarantee real.
- **`lib/kvstore`**: an append-only key/value store that survives a torn
  write. Newest entry wins, a half-written entry fails its checksum and the
  scan stops there, and everything written before it is untouched. Reports a
  full or corrupt store rather than failing quietly.

  **No flash backend yet.** It talks to a `Storage` trait, which nothing on the
  ESP32 implements so far, so nothing is actually persisted across a reboot.
  The format and the recovery are done and tested; the flash driver is not.

- **`esp32-timg`**: TIMG0/TIMG1, four independent 64-bit timers with a 16-bit
  prescaler, one-shot and periodic alarms, and a microsecond clock. Verified on
  hardware against the scheduler tick — two independent clocks, neither
  confirming itself.

  Periodic mode is verified too: a handler must re-arm `ALARM_EN` on every
  alarm, because auto-reload puts the counter back and not the alarm. Without
  that the timer fires once and stops, which the on-target test says in those
  words.

  The kernel's `timer::once`/`every` are unchanged and still ride the tick.
  This is the hardware for anything that needs to be accurate rather than
  coarse; nothing has been moved onto it yet.

- **DMA descriptors** (`soc_esp32::dma::Descriptor`, `build_chain`). The
  12-byte linked-list descriptor the engine actually walks, laid out from the
  ROM header, with the buffer's reachability and alignment checked before an
  address can reach one. No transfer engine yet — this is the piece the
  register programming will need, not the programming.

- **[Multicore](https://github.com/cooljackal/flintos/wiki/Multicore) in the
  wiki.** Starting the second core, why its entry has to be in `.iram1`, what
  is shared and what is per-core, when to pin a task, and why asymmetric cores
  are out of scope.

- **DMA channel allocator** (`soc_esp32::dma`). Three channels shared by SPI1,
  SPI2 and SPI3; a second claim returns an error rather than letting two
  drivers write each other's descriptors.
- **Both cores run the scheduler.** `kernel::boot::join_scheduler` gives a
  secondary core a vector table, a pinned idle task and its own timer, after
  which it takes traps and runs tasks like the first.
- **`task::spawn_on(core, ...)`** pins a task to one core; `spawn` still means
  "either". The scheduler tracks a current task per core and skips a task
  pinned elsewhere. Pinning to a core that does not run the scheduler is
  refused rather than silently accepted.
- **The kernel is safe for two cores.** `kernel::smp::Spinlock` excludes the
  other core as well as this core's interrupts, and the scheduler lives behind
  one. There is no longer any way to reach the scheduler without the lock — the
  `unsafe global()` escape hatch is gone rather than documented.
- **The APP CPU can be started** (`soc_esp32::appcpu`, `arch_xtensa::appcpu`),
  and stalled again — which is what lets a flash write disable both caches
  without stopping the other core mid-instruction.
- **`esp32-ledc`**: PWM output. Eight high-speed channels over four timers,
  with the frequency/resolution arithmetic as pure functions that refuse an
  impossible combination rather than clamping it.
- **`mpu6886`**, a Layer-3 driver for the Atom Matrix's onboard IMU:
  acceleration, angular rate and die temperature, in integer milli-units. The
  first device in this tree driven through all three layers.
- **`lib/led-strip`**: what an addressable LED strip promises, and effects
  written once against it rather than once per chip. `ws2812` implements
  `LedStrip`; it deliberately does not implement `Dimmable`, because these
  parts have no brightness register.
- **`make device-matrix`** prints which drivers keep which device-class
  promise, so "this chip cannot do it" and "nobody got round to it" stop
  looking identical.
- **`lib/`**, a home for portable libraries that are not drivers: no
  registers, no part numbers, output is a value rather than something bound for
  a pin. `tools/check-layers.sh` enforces that they depend only on `api` and on
  each other.
- **`led-matrix`** (in `lib/`): chained LED panel geometry, `(x, y)` to a
  position along the chain, with the fold described as data. It ships no board
  constants — a panel's layout is a fact about a board, so `board::active`
  declares it alongside the pin.
- **Board manifests declare their LEDs**: `RGB_LED_COUNT` and `RGB_LED_LAYOUT`
  join `RGB_LED_GPIO`, so an application no longer carries the count.
- **`make test-boards`** runs every board manifest's invariant tests. Only the
  selected board's tests ran before, leaving every other manifest unchecked.
- **Peripheral interrupt routing** (`soc_esp32::intr_map`). The DPORT crossbar
  that decides which of the CPU's 32 interrupt inputs a peripheral fires on.
  Nothing routed one before, so every driver's interrupt was unreachable.
- **RMT streaming** (`Rmt::start_stream`): frames longer than the 64-entry
  block, refilled half at a time from the channel's interrupt.
- **RMT feeds the channel through `RMTMEM` rather than the APB FIFO.** Via the
  FIFO, only the first frame transmitted: the write pointer is rewound by
  `APB_MEM_RST`, a different bit from the `MEM_RD_RST` that rewinds the read
  pointer, so every later frame landed past the terminator and the channel
  replayed the first one. An LED stuck on its first colour.
- **RMT driver** (`soc_esp32::rmt`) and a **WS2812/SK6812 logical driver**
  (`ws2812`), so an addressable LED can be driven with the sub-microsecond
  pulse timing it needs. One shot, one channel's memory block — about two LEDs;
  longer strings need refill-on-interrupt.
- **Hardware RNG** as `kernel::rng`. Suitable for backoffs, jitter and test
  seeds; **not** for keys or tokens — the generator is only cryptographically
  useful with the radio running, and FlintOS does not bring the radio up. Said
  plainly in the module docs rather than hidden behind a reassuring name.
- Six on-target tests for task-versus-ISR races, including a queue fed from the
  timer ISR and drained by a task.
- On-target self-test suite: `make test-target` flashes a board and turns the
  serial output into an exit code.
- Host tests for priority inversion and queue races, and `make test-host` now
  covers the kernel itself.
- `make size` — where an image's bytes went, per memory region.
- `make upgrade` — pull, rebuild every application, report which broke.
- GPIO-matrix pin routing (`PinMux`), so any signal reaches any pad.
- An `arch` / SoC / board split, and the wiki that documents each.

### Fixed

- Priority inheritance now follows a chain of blocked owners instead of one
  link, which is the difference between bounded and unbounded inversion.
- `Queue::send_isr` wakes a blocked receiver. It never did, so a driver task in
  `recv` slept forever with its data already in the ring.
- A task returning from its entry function no longer strands every other task
  at its priority level.
- Register windows are spilled before the trap entry moves the stack pointer.
  This was the long one.

[Unreleased]: https://github.com/cooljackal/flintos/compare/main...HEAD
