---
title: Writing a driver
---


Pick your layer first. Getting this wrong is the one mistake the build will
catch for you.

| You are writing | Layer | Directory | Package name | May depend on |
|---|---|---|---|---|
| A sensor, display, radio | 3 — logical | `drivers/logical/<device>/` | `<device>` | `api` **only** |
| A protocol wrapper | 2 — bus | `drivers/bus/<proto>-bus/` | `<proto>-bus` | `api` **only** |
| A peripheral register driver | 1 — physical | `drivers/physical/<chip>/<periph>/` | `<chip>-<periph>` | `hal`, `soc-*` |

The package name is the directory leaf, with no prefix — `drivers/logical/bme280/`
is `bme280`.

Layer 1 is the exception, and deliberately. Those crates are grouped by the SoC
they are bound to, so `drivers/physical/esp32/i2c/` is `esp32-i2c` — directory
`esp32` plus leaf `i2c`. The chip stays in the *name* because a dependency line
shows the name without the path, and a bare `i2c` would claim a great deal more
than one chip's controller.

The SoC is the unit of portability down here: every crate under `esp32/`
depends on `soc-esp32` and none of them run anywhere else. Grouping by chip
makes "what does supporting a new SoC involve" answerable by listing one
directory.

Nothing in this tree is published: every package sets `publish = false`, so there
is no global namespace to be unambiguous in. Unrelated crates named `bme280` and
`ssd1306` exist on crates.io and it costs us nothing, because these never go
there. A prefix would buy no clarity and make every path-dependency longer.

Two checks run in CI and fail the build, and a third reports:

- `tools/check-layers.sh` — a whitelist per tier. Layer 2 and 3 may name `api`
  and `lib/*`; Layer 1 may name `hal` and `soc/*`; `soc/*` and `arch/*` may name
  only `hal`; `lib/*` may name only other `lib/*`. Anything else is a violation.
- `tools/check-names.sh` — every package must set `publish = false`, must not
  carry a `flint-` prefix, and its name must match its directory as in the table
  above.
- `make device-matrix` — which drivers keep which device-class promise. Reports
  only; a chip that cannot do a thing must not break the build for saying so.

**The layer check reads the dependency graph, so it cannot stop you writing to a
register directly** — raw MMIO needs no dependency. That is why every logical
driver carries `#![cfg_attr(not(test), forbid(unsafe_code))]`. Put it in yours.

## Layer 3 — a device

Knows a part number, not a chip. Works on any MCU FlintOS supports, unchanged.

If a device class already exists in `lib/` — `LedStrip`, and more later —
implement it rather than inventing an interface. That is what lets an
application swap your chip for another one by changing a single line. Implement
only the traits your hardware genuinely keeps: leaving one out is a statement,
and `make device-matrix` shows it.

```rust
#![no_std]
#![cfg_attr(not(test), forbid(unsafe_code))]

use api::bus::{BusHandle, BusResult};

pub struct Bme280<'a> {
    bus: BusHandle<'a>,
}

impl<'a> Bme280<'a> {
    // Take anything that converts into a handle, so a caller passes a plain
    // `&bus` — `(&bus).into()` builds the `BusHandle`.
    pub fn new(bus: impl Into<BusHandle<'a>>) -> Self {
        Self { bus: bus.into() }
    }

    pub fn read_id(&self) -> BusResult<u8> {
        // Address, then value, in one transaction — the same bytes on SPI
        // and I2C. `BusHandle` dispatches on the bus kind for you.
        self.bus.read_reg(0xD0)
    }
}
```

You get a `BusHandle` and never learn what's behind it — `read_reg`,
`read_regs`, `write_reg`, `read` and `write` all build the transfer and hand
it to the bus. `BusHandle<'a>` borrows the bus rather than owning a `'static`,
so a board can hand you one that lives on the stack; a `new(impl Into<BusHandle>)`
constructor is then called as `Bme280::new(&bus)`, with no `BusHandle::new` at
the call site. If you find yourself wanting a register address or a pin number,
you're in the wrong layer. The shipped `mpu6886` and `bme280` drivers
(`drivers/logical/*/src/lib.rs`) are the fuller worked examples.

## Layer 2 — a bus

Turns a physical driver into a protocol. Implement `api::bus::Bus`:

```rust
fn transfer(&self, ops: &mut [Op]) -> BusResult<()>;   // one transaction
fn max_transfer(&self) -> usize;                       // FIFO-bounded op size
fn kind(&self) -> BusKind;                             // Spi / I2c / …
fn set_speed(&self, speed: BusSpeed) -> BusResult<()>; // default: InvalidConfig
```

A `Bus` runs a *list* of `Op`s as one logical transaction — each `Op` is a
write, a read, or a full-duplex exchange, carrying its own word width,
chip-select hold (`CsHold`) and trailing delay. The old copy-based
`write`/`read`/`transfer(tx, rx)`/`select`/`deselect` calls live on
`BusHandle` now, as thin wrappers that build a one- or two-element `[Op]`, so
logical drivers kept working unchanged. Framing and retries live here.
Registers don't.

The wrapper **owns** its physical driver by value, behind a kernel mutex, so
one `Once<SpiBus<Esp32Spi>>` holds the whole stack with no `&'static dyn` and
no `static mut`. It is generic over any `PhysicalTransfer`:

```rust
let spi = SpiBus::new(esp32_spi);                 // SpiBus<Esp32Spi>
let i2c = I2cController::new(esp32_i2c);           // I2cController<Esp32I2c>
let dev = i2c.device(0x68);                        // I2cDevice<'_> — the Bus a driver talks
```

An `I2cController` addresses the whole bus; `.device(addr)` borrows it for one
slave and *is* the `Bus` a logical driver is handed (`.scan(..)` walks the
address space). `drivers/bus/spi-bus` and `drivers/bus/i2c-bus` are the two
shipped examples.

## Layer 1 — a peripheral

The only layer that touches hardware. The physical driver is split across two
traits, both in `hal::bus`:

```rust
// The run-time half: everything takes &self, so a shared &driver is itself a
// PhysicalTransfer (there is a blanket impl for &T).
trait PhysicalTransfer {
    fn exchange(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()>;
    fn set_speed(&self, speed: BusSpeed) -> BusResult<()>;   // default: InvalidConfig
}

// The construction-time half: the one &mut self method, added on top.
trait PhysicalBus: PhysicalTransfer {
    fn init(&mut self, config: &BusConfig) -> BusResult<()>;
}
```

`exchange` (renamed from the old `raw_transfer`) is the one primitive that
moves bytes: it clocks `tx` out while filling `rx`. **For I2C, `tx[0]` is the
device's 7-bit address, unshifted** — the physical driver adds the R/W bit.
Splitting the `&mut self` construction step onto `PhysicalBus` is what lets a
bus layer own a driver by value and still call `exchange` through a shared `&`.

**Constructing one.** The safe entry point is `Esp32{I2c,Spi,Uart}::open(&Port)`
— it wins the controller's claim flag (a second `open` on the same controller
returns `BusError::Busy`), then does exactly what `init` does from the port's
config. The claim proves single ownership, so there is no `unsafe` at the call
site; the `unsafe fn new(base_addr)` stays for the kernel self-tests.

```rust
use soc_esp32::{I2cCtrl, I2cPort};

let port = I2cPort { ctrl: I2cCtrl::I2c0, cfg: /* I2cConfig */ };
let driver = Esp32I2c::open(&port)?;               // claimed, clocked, routed
```

Behind `open`, `init` does three things, in this order:

```rust
fn init(&mut self, config: &BusConfig) -> BusResult<()> {
    match config {
        BusConfig::I2c(I2cConfig { sda, scl, speed }) => {
            let instance = addr::i2c_instance(self.base).ok_or(BusError::InvalidConfig)?;

            // 1. Ungate the clock. Without this every register write below
            //    lands nowhere, with no fault -- it looks exactly like a
            //    wrong register map.
            let clk = dport::clock_bit(self.base).ok_or(BusError::InvalidConfig)?;
            unsafe { dport::enable(clk) };

            // 2. Route the pins, before programming timing. A configured,
            //    running controller on a still-default pad can drive a bus
            //    another device is holding low.
            route_pins(instance, *sda, *scl)?;

            // 3. Program the peripheral.
            self.half = (APB_HZ / speed.hz() / 2).max(10);
            unsafe { self.program() };
            Ok(())
        }
        _ => Err(BusError::InvalidConfig),
    }
}
```

### Routing pins

```rust
let mux = Esp32PinMux::new();
mux.can_route(sda_sig, sda)?;      // check every pin...
mux.can_route(scl_sig, scl)?;
mux.route(sda_sig, sda, PinConfig::OPEN_DRAIN_PULLUP)?;   // ...then route
mux.route(scl_sig, scl, PinConfig::OPEN_DRAIN_PULLUP)?;
```

Check all, then route all. Routing isn't transactional, and a bus with one line
connected and the other dangling is harder to diagnose than one that refused to
start.

Use `PinConfig::OPEN_DRAIN_PULLUP` for I²C, `PUSH_PULL` for everything else. The
internal pull-up is tens of kΩ — fine for one device on a short bus, not a
substitute for real external pull-ups.

### Register constants

Put addresses and IRQ numbers in the SoC crate, not the driver. Offsets and bit
fields stay in the driver, with the source noted:

```rust
// Confirmed against esp-idf `soc/i2c_reg.h`.
const I2C_CTR: u32 = 0x04;
const I2C_MS_MODE: u32 = 1 << 4;
```

Then assert them in a unit test. These run on the host, in CI, and they are the
only check on a table you can't otherwise verify without an oscilloscope:

```rust
#[test]
fn ms_mode_bit_is_4_not_1() {
    assert_eq!(I2C_MS_MODE, 1 << 4);
}
```

That's not ceremony. Every one of those tests in this tree exists because the
value was wrong once.

## Registering it

Add to the workspace `members`, then to the board manifest's `TARGET_BUSES` or
`TARGET_DEVICES`. See [Adding a Board](/developers/adding-a-board/).

Adding it to `members` is what puts it in CI: the host jobs select the whole
workspace minus the few crates that need the Xtensa toolchain, so a new driver
is checked, tested and linted from the day it lands. Run `make check-names`
before you push — it catches a mistyped package name in about a second, which
is faster than a CI round-trip.

Nothing needs to depend on your crate for it to be built. The kernel names only
the console UART; an application that wants your driver declares it itself.

## Interrupts

Top half in the ISR, work in a task. `Queue::send_isr` wakes a blocked receiver:

```rust
static EVENTS: Queue<Event, 16> = Queue::new();

fn isr() {
    let _ = EVENTS.send_isr(Event::DataReady);   // never blocks
}

fn driver_task() {
    loop {
        if let Ok(ev) = queue::recv(&EVENTS, u32::MAX) {
            handle(ev);
        }
    }
}
```
