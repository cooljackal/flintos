---
title: Adding a board
---


One file, plus four one-line edits to register it. If your board uses a chip FlintOS
already supports, that's the whole job — the SoC crate already knows the
peripheral addresses, the IRQ numbers and how to route pins.

> **What belongs in a manifest:** every fact an application would otherwise
> look up in a datasheet — pins, base addresses, IRQs, and the *shape* of
> whatever is attached. If your board has a panel, measure its layout and
> declare it here; `apps/examples/blink` walks a chain one LED at a time so you can see
> which cell lights for which index. Do not guess it — there are 16 plausible
> layouts and the wrong one lights the wrong pixel, which reads as a broken
> panel.

## 1. Copy a manifest

```bash
cp board/src/esp32_devkitc.rs board/src/my_board.rs
```

Edit the name, the tick, and the pins:

```rust
pub const BOARD_NAME: &str = "My Board";
pub const TICK_PERIOD_US: u32 = 1000;
pub const DMA_POOL_BYTES: usize = 8192;

// Which radios the module physically carries. A fact about the hardware, like
// a pin number — not an assumption from the SoC family. An application that
// enables `radio-ble` against a board declaring `HAS_BT = false` fails to
// build rather than failing to connect.
pub const HAS_WIFI: bool = true;
pub const HAS_BT: bool = true;

pub const TARGET_BUSES: &[BusMapping] = &[
    BusMapping {
        name: "uart0",
        kind: BusKind::Uart,
        base_addr: addr::UART0_BASE,     // from the SoC crate, not a literal
        irq: addr::IRQ_UART0,
        dma_capable: true,
        dma_pool_bytes: 512,
        config: BusConfig::uart_8n1(1, 3, 115200),
    },
];
```

`BusConfig` wraps one config struct per bus kind; the `const fn` helpers
(`uart_8n1`, `spi_mode0`, `i2c`) cover the common shapes. Anything else is
`BusConfig::Uart(UartConfig { .. ..Default::default() })` with struct syntax.

Base addresses and IRQs come from `soc_esp32::addr`. Don't paste hex — a
typo in one board file is invisible from every other.

**Only declare buses that exist on your board.** An `i2c0` entry for a device
nobody soldered on is a landmine for whoever wires one up next. An empty
`TARGET_DEVICES` is the honest answer until there's something to put in it.

Onboard hardware goes in as plain constants:

```rust
pub const RGB_LED_GPIO: u8 = 27;

// A pin without the count of what is on it is half a fact. Declaring only the
// pin let an application drive the first LED of a 25-LED panel and look
// correct while 24 stayed dark -- which is why the Atom is two boards.
pub const RGB_LED_COUNT: usize = 25;
pub const RGB_LED_LAYOUT: Option<led_matrix::Layout> = Some(Layout::new(
    5, 5, Origin::BottomRight, Axis::Rows, Order::Progressive,
));
```

### Gather the facts into `BOARD`

`TARGET_*` and the loose `*_GPIO` consts stay (the kernel self-tests and drivers
read them), but the value an **application** reaches for is one
`pub const BOARD: Board` per module, whose fields are `Option`s so an app guards
on the *fact* — `board::BOARD.imu.is_some()` — not a board name:

```rust
pub const IMU_PORT: soc_esp32::I2cPort = soc_esp32::I2cPort {
    ctrl: soc_esp32::I2cCtrl::I2c0,
    cfg: I2cConfig { sda: 25, scl: 21, speed: BusSpeed::Standard100k },
};

pub const BOARD: crate::Board = crate::Board {
    name: BOARD_NAME,
    imu: Some(crate::I2cAttachment { port: IMU_PORT, addr: 0x68 }),   // or None
    rgb_led: Some(crate::RgbLed { gpio: RGB_LED_GPIO, count: RGB_LED_COUNT, layout: RGB_LED_LAYOUT }),
    selftest: SELFTEST_PADS,     // the free loopback pads, as a struct of Options
    console: CONSOLE,            // ConsolePins { tx, rx, baud }
};
```

An application never opens a controller by hand: `board::imu_bus()`,
`board::led()`, `board::loopback_spi()` and `board::console()` open the device
once from `BOARD`, cache it in an `api::Once`, and hand back the same
`&'static`. Fill `BOARD` in and those accessors work; leave a field `None` and
they return `Error::Other` (or `None`) so the app can say why.

## 2. Register it

`board/Cargo.toml`:

```toml
board-my-board = []
```

`board/src/lib.rs` — four one-line edits:

```rust
// 1. the module
#[cfg(feature = "board-my-board")]
pub mod my_board;

// 2. one more term in the counted assert
const SELECTED: usize = /* … */ + cfg!(feature = "board-my-board") as usize;

// 3. one line in the "no board selected" compile_error! list

// 4. an arm in the `active` re-export
#[cfg(feature = "board-my-board")]
pub use my_board as active;
```

Two guards keep exactly one board on. The "no board selected" case is a
`compile_error!` that lists the boards. The "more than one" case is a **counted
assert** — `assert!(SELECTED <= 1, …)` over a `const SELECTED` that sums
`cfg!(feature = …) as usize` per board — so adding a board is one term, not a
pairwise line against every existing board.

Registering a board touches more than one file — the manifest, `board/Cargo.toml`,
the four places in `board/src/lib.rs` above, `kernel/Cargo.toml`, and the
`BOARDS` list in the `Makefile`. That last one matters: `make test-boards` runs
each manifest's invariant tests, and a board missing from the list is a board
whose tests never run.

## 3. Wire the feature through the kernel

Apps no longer forward board features (#120). The board is the **kernel's**
feature and the build passes it on the command line — `make flash BOARD=…`
turns into `--features kernel/board-my-board`, so nothing goes in the app's
`Cargo.toml`.

`board/Cargo.toml`:

```toml
board-my-board = ["dep:soc-esp32"]   # or dep:soc-rp2040
```

`kernel/Cargo.toml` — pull in the arch, the SoC and the board manifest:

```toml
board-my-board = ["arch-xtensa", "soc-esp32", "board/board-my-board"]
```

## 4. Build

```bash
make flash BOARD=board-my-board
```

## Picking pins

Check [ESP32](/hardware/soc-esp32/) before you commit to anything:

- **Never GPIO 6–11.** SPI flash.
- **GPIO 34–39 are input-only.** No output driver at all. Routing an output
  there returns `InvalidConfig`.
- **Strapping pins** — 0, 2, 5, 12, 15. GPIO12 pulled high at boot sets the
  flash voltage wrong and can brick the module.
- **PSRAM** on WROVER-class modules takes 16 and 17.

Any pin can carry any signal via the GPIO matrix. Native pads are faster and
required for SPI at top speed — the table is on the SoC page.

Validate before you trust a manifest:

```rust
let mux = Esp32PinMux::new();
assert!(mux.can_route(Signal::I2cSda(0), 26).is_ok());
```

`can_route` is pure — no registers touched — so it works in a unit test.

## A board on a chip FlintOS doesn't support yet

That's a new SoC crate, not a board file. See
[Architecture](/developers/architecture/#hardware-arch--soc--board). You'd write `soc/<chip>`
with the peripheral map and a `PinMux` impl, and reuse the arch crate if the
core is the same.
