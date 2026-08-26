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

- **Owned RP2040 programmed I/O** (#175). A portable instruction/state-machine
  contract and `board::programmable_io` construction keep native instructions
  and registers inside the physical driver. The initial polled implementation
  owns each block, program memory and the board's GP2/GP3 pair; it rejects
  collisions and cancels timed-out work before resources are reused. Pico
  loopback passes at 12 and 125 MHz. This does not add CAN, I²S, SDIO, DMA/IRQ
  transfers or arbitrary parallel-pin programs. See [PIO acceptance](doc/rp2040-pio-acceptance.md).

- **RP2040 CPU frequency measurement** (#174, Pico-verified at 12 and 125 MHz).
  The existing SoC contract now uses an exclusively owned, bounded
  hardware counter; ARM boot uses its result for SysTick and labels a failed
  measurement's configured fallback explicitly. No application API change.
  `make test-arm-clock` runs a dual-core nonce-judged fixture; set `ARM_CLOCK_HZ`
  to `12000000` or `125000000`. See [clock acceptance](doc/rp2040-clock-acceptance.md).

- **RP2040 boards and owned peripheral drivers** (#125, #143, #168–#172).
  Select `BOARD=board-raspberry-pi-pico` or `board-wio-rp2040-mini`; applications
  keep ABI 2 and use board construction rather than naming physical drivers.
  UART/GPIO, DMA, PWM/timers, ADC, best-effort entropy, I²C/SPI and flash/KV have
  Pico acceptance coverage. `board::expansion_i2c()` / `expansion_spi()` return
  the existing portable bus wrappers; initialize once during board setup.
  Transfers are polled, SPI chip-select is caller-managed, and address-only
  I²C scans are unsupported. See [bus acceptance](doc/rp2040-bus-acceptance.md).
- **Native RP2040 USB CDC and development reset/update transport** (#172).
  Enable `board/native-usb`, use `board::usb_init(UsbIdentity)`, and service the
  returned `UsbSerial` at least every millisecond from a boot-core task. Reset
  permission is opt-in; act on an acknowledged request through `board::usb_reset`
  in task context. `hal::usb::DeviceController` (also `api::usb`) separates packet
  hardware from the portable device layer. USB CDC uses `ByteStream`, where a
  zero write can mean disconnected as well as full. USB selects a configured
  125 MHz CPU profile and reserves GPIO15/16 on B0/B1 silicon; it cannot share
  GP16 with expansion SPI. USB host and other classes remain unimplemented.
  See [USB acceptance](doc/rp2040-usb-acceptance.md) for identity and safety limits.
- **`api::dma::begin_pair`** checks ownership of both transfer buffers before
  assigning a completion ID. It does not program hardware; the peripheral
  driver still owns that operation.

- **`api::sync::Once<T>` and `api::sync::CsCell<T>`** (#104). `Once` is a
  write-once static: `init` returns a `&'static T` and panics on a second
  init; `get` and `get_or_try_init` for the other shapes. `CsCell::with`
  masks interrupts around a closure for a value a task and an ISR share.
  Both replace `static mut X: Option<T>` + `addr_of!`. The state word is a
  `portable_atomic` atomic, so it works on the Cortex-M0+ too.
- **`task::exit()`, the `Task` builder and `task::wait_until`** (#106).
  `task::exit() -> !` ends the calling task instead of parking it in a sleep
  loop. `Task::new(name, entry).priority(..).stack(..).on_core(..).spawn()`
  defaults to `Priority::Normal(1)` and a 4 KiB stack; the result is
  `#[must_use]`, so a task that failed to start is no longer silent.
  `wait_until(cond, timeout_ms)` polls a condition with a timeout over the
  kernel tick. The existing `spawn` / `spawn_on` are unchanged. Additive, so
  the ABI stays at 1.
- **DAC, ADC2, CAN (TWAI) and I2S drivers, each with an on-chip loopback
  self-test, verified on a DevKitC.** Every one proves itself with no external
  hardware: the DAC drives GPIO25/26 and ADC2 reads it back on the same pad,
  ADC2 refuses a read while the radio owns SAR2, TWAI transmits and
  self-receives a frame in self-test mode, and I2S DMAs a buffer through a
  one-pad loopback (its `sig_loopback` shares only the clocks, so the serial
  data loops over a pad). `make test-target` now reports 36 on-target tests
  passing, 1 skipped. No absolute accuracy, real bus, or second node is
  claimed. (#35, #75, #27, #26)

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

- **A station connects with WPA2-PSK**, through a supplicant of our own. It
  scans, associates and runs the 4-way handshake to a WPA2/WPA3-transition AP —
  verified on a WROOM-32, which held the link for minutes. The handshake and key
  derivation live in `lib/wpa` over `lib/crypto`, first-party Rust; the
  Espressif blob (libnet80211/libpp) provides MAC and PHY only, not a vendored C
  supplicant. **No IP layer yet** — no DHCP, no sockets, no keepalive or
  GTK-rekey handling, so the link drops at the AP's inactivity timeout
  ([#74](https://github.com/cooljackal/flintos/issues/74)). Associating and
  authenticating is done; joining and staying online is next.

- **`apps/wificonnect`** joins a WPA2 network. The SSID and passphrase come from
  `FLINT_WIFI_SSID` / `FLINT_WIFI_PASS` in the environment at build time.

- **`BusHandle::write_reg`, `read_regs` and `kind`.** The register-mapped
  pattern every sensor driver was hand-rolling, done right for the bus
  underneath: over SPI a burst read is one framed exchange of address plus
  payload, over I2C it is a write then a repeated-start read. `Bus` gained a
  required `kind() -> BusKind` so the handle can tell; in-tree bus wrappers
  and test mocks implement it. Not an ABI bump — no application implements
  `Bus`, and nothing an application calls changed signature.

### Breaking

- **RP2040 images reserve the last 16 KiB of their 2 MiB flash for NVS** (#171).
  Keep firmware/custom partitions out of offsets `0x1fc000..0x200000` and follow
  the [flash acceptance contract](doc/rp2040-flash-acceptance.md). The destructive
  fixture erases that region; normal image installation does not migrate old
  user data. `kernel::nvs::FlashStorage::nvs()` is now runtime-only, not `const`:
  move any constant construction into initialization. `board::nvs_flash()` owns
  driver selection and can refuse an open; duplicate RP2040 opens are rejected.
  ESP32 retains its existing partition and physical flash implementation.

- **ABI 2. `BusConfig`'s variants are now named structs, and `PhysicalBus`
  split into `PhysicalTransfer` (`&self` traffic) + `PhysicalBus` (`&mut init`).**
  An application that builds a bus config or names the physical trait must
  change; declare `abi = 2`.

  Build configs through the helpers, or with struct syntax and `..Default`:

  ```rust
  // was:
  let cfg = BusConfig::Uart { tx: 1, rx: 3, baud: 115_200,
      data_bits: UartDataBits::Bits8, parity: UartParity::None,
      stop_bits: UartStopBits::Stop1 };
  // now:
  let cfg = BusConfig::uart_8n1(1, 3, 115_200);
  // also: BusConfig::spi_mode0(mosi, miso, sck, max_speed), BusConfig::i2c(sda, scl, speed).
  // Matching a variant now binds a struct: BusConfig::Spi(SpiConfig { .. }).
  ```

  The transfer method is renamed and lives on the new supertrait: call
  `PhysicalTransfer::exchange(&self, tx, rx)` where you called
  `PhysicalBus::raw_transfer`. `set_enabled` is gone (it was a no-op on every
  driver), and `impl PhysicalBus for Esp32Gpio` is gone (GPIO is not a bus —
  every transfer returned `Err`). esp32-spi's inherent 64-byte `transfer` is
  now `fifo_exchange`, returning `Err(InvalidConfig)` past 64 bytes.

- **Layer-2/3 bus surface: by-value wrappers, an I²C controller/device split,
  `BusHandle<'a>`, and shared-bus locking** (#114, #115, #116). Still ABI 2 —
  `BusHandle`'s runtime layout is unchanged (a lifetime is zero-cost), and only
  code that names these types changes; declare `abi = 2`.

  - `SpiBus<P: PhysicalTransfer>` and the new `I2cController<P>` own the
    physical driver **by value**: `SpiBus::new(phys)` / `I2cController::new(phys)`,
    not `new(&'static dyn PhysicalBus)`. One `Once<SpiBus<Esp32Spi>>` now holds
    the whole stack. A `&'static` driver still works — `&T` is a
    `PhysicalTransfer` — so `SpiBus::new(dev)` compiles unchanged.
  - `I2cBus` is gone. An `I2cController` hands out an `I2cDevice` per slave
    (`ctrl.device(addr)` is the `Bus`) and scans the bus (`ctrl.scan(|a| ..)`),
    so a scan or a second device no longer bypasses Layer 2.
  - `BusHandle` is now `BusHandle<'a>`, so a driver can borrow a bus off the
    stack — no more leaking into a `static`. `impl From<&B> for BusHandle`
    means `Mpu6886::new(&bus)` instead of `Mpu6886::new(BusHandle::new(&bus))`;
    logical-driver types gained a lifetime (`Mpu6886<'a>`, `Bme280<'a>`, …).
    `BusHandle::select`/`deselect` (always-`Ok`) are removed — chip-select is
    per-`Op`.
  - `SpiBus`/`I2cController` take an `api::mutex` lock around the transfer, so
    two tasks sharing one `&'static` bus serialize. That lock is a syscall the
    kernel refuses from interrupt context: **do not call `transfer` from an
    ISR** — an ISR owns the physical driver directly (`exchange` is `&self`).
  - `mpu6886` gains `bring_up(delay_ms)`, which owns the reset/wake/configure
    sequence and its 10 ms waits (the caller passes `api::task::sleep_ms`).

### Changed

- **The CPU runs at 240 MHz.** The bootloader hands off at its 80 MHz default;
  `soc/esp32/cpu_clk::set_240mhz`, called from `kernel/boot.rs`, raises it — the
  clock Espressif builds and times the Wi-Fi blob against. APB stays fixed at
  80 MHz, so UART and the timers are unaffected. Boot still measures the clock,
  and now reads 240 MHz rather than 80.

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

- **ARM integration gates** (#149): `make test-host` includes Pico and Wio
  manifests; `make check-all` checks both ARM test applications against both
  boards. CI links `usb-selftest` for ARM and `touch` for its required Core2.
- **Remote task publication and timer scanning**: spawning now notifies the
  destination core after publishing the complete context; timer processing
  snapshots due callbacks under one lock and invokes them outside that lock.
  Host tests and RP2040 task/ISR fixtures cover these shared kernel paths.
- **RP2040 watchdog routing and retained panic reporting**: panic recovery
  uses the SoC's `hal::reset::PanicRecovery` contract; the unsupported default
  leaves ESP32 halted as before. Console writes give up after bounded stalled
  polling. Watchdog fault fixtures prove explicit recovery paths, not continuous
  supervision of every application. CPU measurement and MPU isolation remain
  unimplemented; see the [capability audit](doc/rp2040-capability-audit.md).

- **A register read over SPI returned one byte, whatever was asked for**
  (#97). `BusHandle::read_reg` and the drivers' `transfer(&[reg], buf)`
  exchanged the 1-byte address against the N-byte buffer, and SPI clocks
  only as many bytes as the shorter side. Only I2C ever got the burst.
  `read_reg` now goes through `read_regs`, and bme280 and mpu6886 use the
  handle's register helpers; bmi270 was already on `read_reg`.

- **An SPI write or read longer than 64 bytes was silently cut to 64**
  (#98). `SpiBus` now clocks the op in FIFO-sized pieces; `I2cBus`, whose
  frame cannot be split, refuses an over-long write with `InvalidConfig`
  instead of clipping it.

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
