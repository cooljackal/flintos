# M5Stack Atom

ATOM Lite and ATOM Matrix. Both are ESP32-PICO-D4.

```bash
make flash BOARD=board-m5-atom-matrix   # or board-m5-atom-lite
```

Features: `board-m5-atom-lite` (1 LED) and `board-m5-atom-matrix` (5x5 panel).
Manifests: `board/src/m5_atom_lite.rs`, `board/src/m5_atom_matrix.rs`, sharing
`board/src/m5_atom_common.rs`.

The two are the same board with a different LED count, and one feature could not
tell them apart -- an application told only the pin drove the first LED of the
panel and left 24 dark.

SoC: ESP32 — see [ESP32](SoC-ESP32) for the full pin table and peripheral map.
Tick: 1 ms.

## The Matrix panel

25 SK6812 on GPIO 27, driven over RMT. **Measured, not read off a datasheet:**

| | |
|---|---|
| Index 0 | bottom-right corner |
| Direction | right to left along a row |
| Rows | bottom to top |
| Order | progressive — each row restarts at the right, *not* a zigzag |

Progressive is the less common arrangement, so guessing would have got it
wrong. Equivalently the panel is an ordinary top-left row-major layout rotated
180°, and `board/src/m5_atom_matrix.rs` asserts both formulations.

25 LEDs is 600 RMT entries against a 64-entry memory block, so this board
cannot be driven without `Rmt::start_stream` — refill-on-interrupt. One channel
block would reach two LEDs.

```bash
make flash APP=blink BOARD=board-m5-atom-matrix
```

Draws a column sweeping right, a row sweeping down, then a diagonal, then walks
the chain logging each index. A correct layout draws straight lines; a wrong one
scatters the same lit cells.

**This is the board Flint's bring-up was done on.** Boot, scheduling,
preemption, timed wakeup and the register-window fix were all verified here.

## The PICO-D4 difference

The flash is inside the SiP, but it still uses **GPIO 6–11** — same restriction
as every other ESP32. No PSRAM, so **GPIO16 and GPIO17 are free**
(`PSRAM_FREE_GPIOS` in the manifest).

## Onboard hardware

| What | GPIO | Constant |
|---|---|---|
| RGB LED (SK6812, single-wire) | 27 | `RGB_LED_GPIO` |
| Button | 39 | `BUTTON_GPIO` |
| Grove port SDA | 26 | `GROVE_SDA_GPIO` |
| Grove port SCL | 32 | `GROVE_SCL_GPIO` |

These are plain constants, not bus entries — no driver in this tree drives them
yet. Reach for them directly:

```rust
use board::active::{RGB_LED_GPIO, BUTTON_GPIO};
```

GPIO39 is input-only, which is fine for a button.

## What the manifest wires up

| Bus | Peripheral | Pins | Speed |
|---|---|---|---|
| `uart0` | UART0 | TX 1, RX 3 | 115200 8N1 |

UART0 only. No SPI or I²C entry, because the Atom has no onboard BME280 or
SSD1306 — those are WROVER dev-board wiring. Inventing bus entries for hardware
that isn't there is exactly the copy-paste error the manifest tests exist to
catch.

To use the Grove port as I²C, add an `i2c0` `BusMapping` with SDA 26 / SCL 32
and the I2C0 base address. The GPIO matrix reaches those pins fine — ESP32 I²C
has no native pads anyway.

## Free pins

Broken out on the Atom's headers: **19, 21, 22, 23, 25, 33** plus the Grove pins
**26, 32** and the reserved-but-free **16, 17**.

Careful with **0, 2, 5, 12, 15** — strapping pins. GPIO12 especially: pulling it
high at boot sets the flash voltage wrong and can brick the module.

Never **6–11** — SPI flash.

## Flashing

The reset button is the **small button on the side**, not the big face button
(that one is GPIO39). If the board won't enter download mode, hold GPIO0 low,
tap side-reset, release. See [Troubleshooting](Troubleshooting).
