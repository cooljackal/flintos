# ESP32

Classic ESP32 (and the PICO-D4 SiP built on it). Xtensa LX6 dual-core; Flint
uses core 0 only.

Crate: `soc/esp32` (`soc-esp32`). Everything below is in code — `addr.rs`,
`io_mux.rs`, `gpio_matrix.rs`, `dport.rs`, `pinmux.rs`, `intr_map.rs`,
`reset.rs`, `app_desc.rs`.

**Infrastructure only.** This crate holds what every peripheral driver needs
underneath it — the address map, pin routing, clock gating, the interrupt
crossbar. A peripheral itself is a driver: RMT, the watchdogs and the RNG were
modules here and are now `drivers/physical/esp32-rmt`, `-wdt` and `-rng`. The
test is whether a *second* peripheral driver would want it.

## Pins

**Never use GPIO 6–11.** They are the SPI flash the chip is executing from.

| GPIO | Safe? | Notes |
|---|---|---|
| 0 | strapping | Boot mode. Pulled low = download mode. ADC2_1, touch1 |
| 1 | ✅ | U0TXD — the console |
| 2 | strapping | Must be low/floating to enter download. ADC2_2, touch2 |
| 3 | ✅ | U0RXD — the console |
| 4 | ✅ | ADC2_0, touch0 |
| 5 | strapping | SDIO timing. VSPICS0. Outputs a PWM at boot |
| 6–11 | ⛔ | **SPI flash. Using these bricks the running image** |
| 12 | strapping | **Flash voltage. Pulling high at boot can brick the module.** MTDI, HSPIQ, ADC2_5, touch5 |
| 13 | ✅ | MTCK, HSPID, ADC2_4, touch4 |
| 14 | ✅ | MTMS, HSPICLK, ADC2_6, touch6 |
| 15 | strapping | Pull low to silence the ROM boot log. MTDO, HSPICS0, ADC2_3, touch3 |
| 16 | board | Free on most; **PSRAM CS on WROVER**. U2RXD |
| 17 | board | Free on most; **PSRAM CLK on WROVER**. U2TXD |
| 18 | ✅ | VSPICLK |
| 19 | ✅ | VSPIQ (MISO) |
| 20 | ⛔ | Not bonded out on WROOM/WROVER/PICO-D4 |
| 21 | ✅ | Conventional I²C SDA |
| 22 | ✅ | Conventional I²C SCL |
| 23 | ✅ | VSPID (MOSI) |
| 24 | ⛔ | Not bonded out on WROOM/WROVER/PICO-D4 |
| 25 | ✅ | ADC2_8, DAC1 |
| 26 | ✅ | ADC2_9, DAC2 |
| 27 | ✅ | ADC2_7, touch7 |
| 28–31 | ⛔ | Do not exist |
| 32 | ✅ | ADC1_4, touch9, 32 kHz XTAL |
| 33 | ✅ | ADC1_5, touch8, 32 kHz XTAL |
| 34 | input only | ADC1_6. **No output driver** |
| 35 | input only | ADC1_7. No output driver |
| 36 | input only | ADC1_0 (SENSOR_VP). No output driver |
| 37 | input only | ADC1_1. Not bonded on most modules |
| 38 | input only | ADC1_2. Not bonded on most modules |
| 39 | input only | ADC1_3 (SENSOR_VN). No output driver |

**ADC2 is unusable while WiFi is on.** ADC1 (GPIO 32–39) is always available.

Routing an output signal to 34–39 returns `BusError::InvalidConfig` — the
hardware wouldn't complain, the pin would just sit wherever the board pulls it.

## Pin routing

The ESP32 has a **GPIO matrix**: almost any peripheral signal reaches almost any
pad. A few high-speed signals also have "native" pads that bypass the matrix for
lower latency.

```rust
use hal::pinmux::{PinConfig, PinMux, Signal};
use soc_esp32::Esp32PinMux;

let mux = Esp32PinMux::new();
mux.can_route(Signal::I2cSda(0), 21)?;      // pure check, no registers touched
mux.route(Signal::I2cSda(0), 21, PinConfig::OPEN_DRAIN_PULLUP)?;
```

Check every pin with `can_route` before routing any — routing isn't
transactional, and a bus with SDA connected and SCL dangling is harder to
diagnose than one that refused to start.

### Native pads

| Signal | Pad | IO_MUX function |
|---|---|---|
| UART0 TX / RX | 1 / 3 | 0 |
| UART1 TX / RX | 10 / 9 | **4** |
| UART2 TX / RX | 17 / 16 | **4** |
| SPI2 (HSPI) MOSI / MISO / SCK / CS | 13 / 12 / 14 / 15 | 1 |
| SPI3 (VSPI) MOSI / MISO / SCK / CS | 23 / 19 / 18 / 5 | 1 |
| I²C | — | **none exist** |

UART1/UART2 are function 4, not 0 — function 0 on those pads is SD_DATA2/3 and
plain GPIO. I²C has no native pads at all on this chip, which is why every ESP32
I²C bus goes through the matrix.

Matrix-routed pads use IO_MUX function **2** (`PIN_FUNC_GPIO`), on every pin
without exception.

### Signal indices

`GPIO_FUNCn_IN_SEL_CFG` is indexed by *signal*; `GPIO_FUNCn_OUT_SEL_CFG` by
*GPIO number*. Easy to mix up.

| Signal | Index |
|---|---|
| UART0 RX / TX | 14 |
| UART0 CTS / RTS | 15 |
| UART1 RX / TX | 17 |
| UART2 RX / TX | 198 |
| I2C0 SCL | 29 |
| I2C0 SDA | 30 |
| I2C1 SCL | 95 |
| I2C1 SDA | 96 |
| SPI2 SCK / MISO / MOSI / CS | 8 / 9 / 10 / 11 |
| SPI3 SCK / MISO / MOSI / CS | 63 / 64 / 65 / **68** |

SPI3 CS is 68, not 66 or 67 — those are HD and WP. The four SPI signals are not
contiguous.

## Peripheral map

| Peripheral | Base | IRQ source |
|---|---|---|
| UART0 | `0x3FF40000` | 34 |
| UART1 | `0x3FF50000` | 35 |
| UART2 | `0x3FF6E000` | 36 |
| SPI1 (boot flash — don't touch) | `0x3FF42000` | 29 |
| SPI2 / HSPI | `0x3FF64000` | 30 |
| SPI3 / VSPI | `0x3FF65000` | 31 |
| I2C0 | `0x3FF53000` | 49 |
| I2C1 | `0x3FF67000` | 50 |
| GPIO | `0x3FF44000` | 22 |
| IO_MUX | `0x3FF49000` | — |
| DPORT | `0x3FF00000` | — |
| RTC_CNTL | `0x3FF48000` | — |
| TIMG0 / TIMG1 | `0x3FF5F000` / `0x3FF60000` | — |

I2C1 is a separate block, **not** I2C0 + 0x20.

## Clock gating

Most peripherals boot clock-gated off and held in reset. Every register access
reads zero and writes nowhere, **with no fault** — so a forgotten ungate looks
exactly like a wrong register map.

```rust
let bit = dport::clock_bit(base).ok_or(BusError::InvalidConfig)?;
unsafe { dport::enable(bit) };
```

`DPORT_PERIP_CLK_EN` = `0x3FF000C0`, `DPORT_PERIP_RST_EN` = `0x3FF000C4`.
Bits: UART0=2, UART1=5, UART2=23, SPI2=6, SPI3=16, I2C0=7, I2C1=18.

UART0 works without this only because the boot ROM already ungated it.

## Clocks

- **APB = 80 MHz**, fixed. Every peripheral divisor derives from this, *not*
  from the CPU frequency.
- **CPU** = 80/160/240 MHz. Flint measures it at boot against the RTC slow clock
  rather than assuming — see the `cpu_hz=` banner line.

## Memory

| Region | Address | Size |
|---|---|---|
| DROM (flash rodata, XIP) | `0x3F400020` | 4 MB window |
| IRAM vectors | `0x40080000` | 1 KB |
| IRAM | `0x40080400` | 127 KB |
| IROM (flash code, XIP) | `0x400D0020` | 3.2 MB window |
| DRAM (data/bss/kernel stack) | `0x3FFB0000` | 64 KB |
| Task stacks | `0x3FFC0000` | 96 KB |
| Panic snapshot | `0x3FFD8000` | 4 KB |
| DMA pool | `0x3FFD9000` | 8 KB |

Everything statically placed must stay below **`0x3FFDC200`** — the ROM keeps
its own data and stack above that during boot.

DMA buffers must live in SRAM2 (`0x3FFAE000`–`0x3FFDFFFF`) to be reachable by
the DMA engines.

Code placed past `0x40400000` is not mapped and faults on fetch.

## Sources

Everything here is checked against esp-idf headers: `soc/soc.h`,
`soc/gpio_reg.h`, `soc/gpio_sig_map.h`, `soc/io_mux_reg.h`, `soc/dport_reg.h`.
The crate's unit tests assert the tables, and they run in CI.
