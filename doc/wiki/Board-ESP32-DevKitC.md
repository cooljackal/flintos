# ESP32-DevKitC / WROOM-32

```bash
make flash BOARD=board-esp32-devkitc
```

There is no default board — every build names one.

Feature: `board-esp32-devkitc`. Manifest: `board/src/esp32_devkitc.rs`.

SoC: ESP32 — see [ESP32](SoC-ESP32) for the full pin table and peripheral map.
Tick: 1 ms.

**This is the reference board for on-target work.** Verified on an ESP32
rev v3.0 module (4 MB flash, dual core, Wi-Fi and BT) — see
[What has actually been run](#what-has-actually-been-run) below.

Note the *default* board is the WROVER, which nobody has flashed. Pass
`BOARD=board-esp32-devkitc` to get the verified path.

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

## What has actually been run

| Check | Result |
|---|---|
| `make test-target BOARD=board-esp32-devkitc` | 32 pass, 0 fail, 1 skip |
| `make flash APP=flashprobe BOARD=board-esp32-devkitc` | erase, program and read back the `nvs` partition, core 1 running throughout |
| `make flash APP=smp BOARD=board-esp32-devkitc` | both cores scheduling, pinned and floating tasks, no lost DPORT writes |
| `make flash APP=demo BOARD=board-esp32-devkitc` | three tasks at three priorities, stable |
| `make flash APP=radioprobe BOARD=board-esp32-devkitc EXTRA_FEATURES=blobs` | PHY registers, full calibration ~183 ms, re-enable ~250 µs, calibration survives a reboot |

### The skipped test

`adc1_follows_the_pin_it_is_pointed_at` needs a pin the *board* holds hard
high, because the chip cannot supply one: a pad in analog mode has its digital
buffers bypassed, and the internal pull-up manages about 4% of full scale
against the SAR's sampling capacitor.

A bare DevKitC has nothing suitable — GPIO34-39 are input-only with **no
internal pull at all**, so they float. The Atoms declare their button, so the
test runs there. To run it here, jumper GPIO39 to 3V3 and set

```rust
pub const ADC_EXTERNAL_HIGH_GPIO: Option<u8> = Some(39);
```

in `board/src/esp32_devkitc.rs`. The suite reports it as `SKIP` rather than
passing or failing, so the count stays honest either way.

### Flash chips vary, and this one is unusual

The module tested here reports JEDEC vendor `0xD8`, which is neither
GigaDevice (`0xC8`) nor Winbond (`0xEF`) — the two layouts the flash driver
knows. That only matters if a chip arrives with its block-protect bits set:
`unlock` then refuses with `FlashError::UnknownChip` rather than writing a
status register whose QE bit it cannot locate, because clearing QE on a
QIO-boot board leaves it unbootable.

Unprotected chips, which is the normal case, are unaffected. `make flash
APP=flashprobe` prints the vendor byte in its first few lines.

## Two bugs this board found

Both had passed every host test, and both were in code that only runs on
silicon:

- **The APP CPU stall did not take effect before it was relied on.**
  `RTC_CNTL` runs on the RTC slow clock, so the stall request crosses a clock
  domain and the other core keeps executing for microseconds. A flash write
  disabled its cache in that window and core 1 executed rubbish —
  `EXCCAUSE=0` inside the scheduler. The unstall had the mirror of it, which
  made a *second* flash operation skip the stall entirely.
- **The ADC test hardcoded another board's button.** It read GPIO39 expecting
  the Atom's external pull-up, and would have failed here on a perfectly
  healthy board.
