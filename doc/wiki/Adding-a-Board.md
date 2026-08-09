# Adding a Board

One file, plus three lines of registration. If your board uses a chip FlintOS
already supports, that's the whole job — the SoC crate already knows the
peripheral addresses, the IRQ numbers and how to route pins.

> **What belongs in a manifest:** every fact an application would otherwise
> look up in a datasheet — pins, base addresses, IRQs, and the *shape* of
> whatever is attached. If your board has a panel, measure its layout and
> declare it here; `apps/blink` walks a chain one LED at a time so you can see
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

pub const TARGET_BUSES: &[BusMapping] = &[
    BusMapping {
        name: "uart0",
        kind: BusKind::Uart,
        base_addr: addr::UART0_BASE,     // from the SoC crate, not a literal
        irq: addr::IRQ_UART0,
        dma_capable: true,
        dma_pool_bytes: 512,
        config: BusConfig::Uart {
            tx: 1, rx: 3, baud: 115200,
            data_bits: UartDataBits::Bits8,
            parity: UartParity::None,
            stop_bits: UartStopBits::Stop1,
        },
    },
];
```

Base addresses and IRQs come from `soc_esp32::addr`. Don't paste hex — a
typo in one board file is invisible from every other.

**Only declare buses that exist on your board.** An `i2c0` entry for a device
nobody soldered on is a landmine for whoever wires one up next. An empty
`TARGET_DEVICES` is the honest answer until there's something to put in it.

Onboard hardware with no driver yet goes in as a plain constant:

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

## 2. Register it

`board/Cargo.toml`:

```toml
board-my-board = []
```

`board/src/lib.rs` — three edits:

```rust
#[cfg(feature = "board-my-board")]
pub mod my_board;
```

Add an arm to the `active` re-export, and extend both `compile_error!` guards.
The "more than one board" guard is pairwise, so it grows with each board.

Registering a board touches more than one file — the manifest, `board/Cargo.toml`,
three places in `board/src/lib.rs`, `kernel/Cargo.toml`, each app's `Cargo.toml`,
and the `BOARDS` list in the `Makefile`. That last one matters: `make test-boards`
runs each manifest's invariant tests, and a board missing from the list is a
board whose tests never run.
The "more than one" guard is pairwise across every board feature, so it grows
by one line per existing board.

## 3. Let apps select it

In each app's `Cargo.toml`:

```toml
board-my-board = ["kernel/board-my-board"]
```

And `kernel/Cargo.toml`:

```toml
board-my-board = ["board/board-my-board"]
```

## 4. Build

```bash
make flash BOARD=board-my-board
```

## Picking pins

Check [ESP32](SoC-ESP32) before you commit to anything:

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
[Architecture](Architecture#hardware-arch--soc--board). You'd write `soc/<chip>`
with the peripheral map and a `PinMux` impl, and reuse the arch crate if the
core is the same.
