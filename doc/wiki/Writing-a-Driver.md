# Writing a Driver

Pick your layer first. Getting this wrong is the one mistake the build will
catch for you.

| You are writing | Layer | Directory | Package name | May depend on |
|---|---|---|---|---|
| A sensor, display, radio | 3 — logical | `drivers/logical/<device>/` | `<device>` | `api` **only** |
| A protocol wrapper | 2 — bus | `drivers/bus/<proto>-bus/` | `<proto>-bus` | `api` **only** |
| A peripheral register driver | 1 — physical | `drivers/physical/<chip>-<periph>/` | `<chip>-<periph>` | `hal`, `soc-*` |

The package name is the directory leaf, with no prefix — `drivers/logical/bme280/`
is `bme280`, `drivers/physical/esp32-i2c/` is `esp32-i2c`.

Nothing in this tree is published: every package sets `publish = false`, so there
is no global namespace to be unambiguous in. Unrelated crates named `bme280` and
`ssd1306` exist on crates.io and it costs us nothing, because these never go
there. A prefix would buy no clarity and make every path-dependency longer.

Two checks run in CI and fail the build:

- `tools/check-layers.sh` — a Layer 2 or 3 crate may name **only** `api`.
  Anything else is a violation, including a Layer 1 driver.
- `tools/check-names.sh` — every package must set `publish = false`, must not
  carry a `flint-` prefix, and its name must match its directory as in the table
  above.

## Layer 3 — a device

Knows a device, not a chip. Works on any MCU Flint supports, unchanged.

```rust
#![no_std]

use api::bus::{BusHandle, BusResult};

pub struct Bme280 {
    bus: BusHandle,
}

impl Bme280 {
    pub fn new(bus: BusHandle) -> Self {
        Self { bus }
    }

    pub fn read_id(&self) -> BusResult<u8> {
        let mut rx = [0u8; 1];
        self.bus.select()?;
        self.bus.write(&[0xD0])?;
        self.bus.read(&mut rx)?;
        self.bus.deselect()?;
        Ok(rx[0])
    }
}
```

You get a `BusHandle` and never learn what's behind it. If you find yourself
wanting a register address or a pin number, you're in the wrong layer.

## Layer 2 — a bus

Turns a `PhysicalBus` into a protocol. Implement `api::bus::Bus`:

```rust
fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()>;
fn write(&self, data: &[u8]) -> BusResult<()>;
fn read(&self, buf: &mut [u8]) -> BusResult<()>;
fn set_speed(&self, speed: BusSpeed) -> BusResult<()>;
fn select(&self) -> BusResult<()>;
fn deselect(&self) -> BusResult<()>;
```

Chip select, framing, retries live here. Registers don't.

## Layer 1 — a peripheral

The only layer that touches hardware. Implement
`hal::bus::PhysicalBus`:

```rust
fn init(&mut self, config: &BusConfig) -> BusResult<()>;
fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()>;
fn set_enabled(&mut self, enabled: bool);
```

`init` does three things, in this order:

```rust
fn init(&mut self, config: &BusConfig) -> BusResult<()> {
    let BusConfig::I2c { sda, scl, speed } = config else {
        return Err(BusError::InvalidConfig);
    };
    let instance = addr::i2c_instance(self.base).ok_or(BusError::InvalidConfig)?;

    // 1. Ungate the clock. Without this every register write below lands
    //    nowhere, with no fault -- it looks exactly like a wrong register map.
    let clk = dport::clock_bit(self.base).ok_or(BusError::InvalidConfig)?;
    unsafe { dport::enable(clk) };

    // 2. Route the pins, before programming timing. A configured, running
    //    controller connected to a still-default pad can drive a bus another
    //    device is holding low.
    route_pins(instance, *sda, *scl)?;

    // 3. Program the peripheral.
    let half = (APB_HZ / speed.hz() / 2).max(10);
    unsafe { self.reg(I2C_SCL_LOW).write_volatile(half) };
    Ok(())
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
`TARGET_DEVICES`. See [Adding a Board](Adding-a-Board).

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
        if let Some(ev) = queue::recv(&EVENTS, u32::MAX) {
            handle(ev);
        }
    }
}
```
