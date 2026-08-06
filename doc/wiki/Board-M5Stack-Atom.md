# M5Stack Atom

ATOM Lite and ATOM Matrix. Both are ESP32-PICO-D4.

```bash
make flash BOARD=board-m5-atom
```

Feature: `board-m5-atom`. Manifest: `board/src/m5_atom.rs`.

SoC: ESP32 — see [ESP32](SoC-ESP32) for the full pin table and peripheral map.
Tick: 1 ms.

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
use flint_board::active::{RGB_LED_GPIO, BUTTON_GPIO};
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
