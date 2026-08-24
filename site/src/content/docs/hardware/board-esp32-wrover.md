---
title: "ESP32-WROVER"
---

```bash
make flash BOARD=board-esp32-wrover
```

Feature: `board-esp32-wrover`. Manifest: `board/src/esp32_wrover.rs`.

SoC: ESP32 — see [ESP32](/hardware/soc-esp32/) for the full pin table and peripheral map.
Tick: 1 ms.

> **Never flashed.** The manifest is written and its invariant tests pass,
> but nobody has held the hardware. This used to be the default board, which
> meant `make flash` with no argument built for the least tested board in the
> tree; there is no default any more. The verified board is the
> [DevKitC / WROOM-32](/hardware/board-esp32-devkitc/).
>
> Same silicon and same image format, so this should work. If you have one,
> please report either way — bringing up the WROOM-32 found two real bugs in
> a day, both of which had passed every host test.

## The WROVER difference

**GPIO16 and GPIO17 are PSRAM.** The module carries 4–8 MB of external PSRAM
wired to those two pins. Using them for anything else breaks it.

That also means **UART2's native pads are gone** on this board — U2RXD/U2TXD are
16/17. Route UART2 through the GPIO matrix to other pins if you need it.

Everything else matches a plain ESP32 module.

## What the manifest wires up

| Bus | Peripheral | Pins | Speed |
|---|---|---|---|
| `uart0` | UART0 | TX 1, RX 3 | 115200 8N1 |
| `spi3` | SPI3 / VSPI | MOSI 23, MISO 19, SCK 18 | 40 MHz, Mode 0 |
| `i2c0` | I2C0 | SDA 21, SCL 22 | 400 kHz |

| Device | Driver | Bus | CS |
|---|---|---|---|
| `temp_sensor` | `bme280` | `spi3` | GPIO15 |
| `display` | `ssd1306` | `i2c0` | — |

The VSPI pins are IO_MUX-native, so that bus bypasses the GPIO matrix.

> The BME280 and SSD1306 are dev-board wiring, not onboard parts. If your WROVER
> board doesn't have them attached, those entries do nothing.

## Free pins

Safe to use, after the reservations above:

**4, 12, 13, 14, 25, 26, 27, 32, 33** and input-only **34, 35, 36, 39**.

Careful with **0, 2, 5, 12, 15** — strapping pins. GPIO12 especially: pulling it
high at boot sets the flash voltage wrong and can brick the module.

Never **6–11** (SPI flash) or **16, 17** (PSRAM).

## Flashing

Standard USB serial. If it fails, see [Troubleshooting](/users/troubleshooting/).
