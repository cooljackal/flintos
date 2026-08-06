# ESP32-DevKitC / WROOM-32

```bash
make flash BOARD=board-esp32-devkitc
```

Feature: `board-esp32-devkitc`. Manifest: `board/src/esp32_devkitc.rs`.

SoC: ESP32 — see [ESP32](SoC-ESP32) for the full pin table and peripheral map.
Tick: 1 ms.

**Untested on hardware.** Same silicon and same image format as the WROVER, so
it should just work. If you have one, please report either way.

## The DevKitC difference

No PSRAM, so **GPIO16 and GPIO17 are free** — unlike the WROVER. That also means
UART2's native pads (17/16) are actually available here.

## What the manifest wires up

| Bus | Peripheral | Pins | Speed |
|---|---|---|---|
| `uart0` | UART0 | TX 1, RX 3 | 115200 8N1 |
| `spi3` | SPI3 / VSPI | MOSI 23, MISO 19, SCK 18 | 40 MHz, Mode 0 |
| `i2c0` | I2C0 | SDA 21, SCL 22 | 400 kHz |

The VSPI pins are IO_MUX-native, so that bus bypasses the GPIO matrix.

## Free pins

**4, 12, 13, 14, 16, 17, 25, 26, 27, 32, 33** and input-only **34, 35, 36, 39**.

Careful with **0, 2, 5, 12, 15** — strapping pins. GPIO12 especially: pulling it
high at boot sets the flash voltage wrong and can brick the module.

Never **6–11** — SPI flash.

## Flashing

Standard USB serial via the onboard CP2102 or CH340. If it fails, see
[Troubleshooting](Troubleshooting).
