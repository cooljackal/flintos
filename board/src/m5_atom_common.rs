// SPDX-License-Identifier: Apache-2.0

// M5Stack ATOM (ESP32-PICO-D4) -- manifest shared by both variants
//
// The Lite and the Matrix are the same board with a different LED count, so
// the pin map, buses and peripherals live here once and each variant module
// re-exports them alongside its own panel declaration. Duplicating this file
// would mean a pin fixed in one copy and left wrong in the other.
//
// BOARD_NAME, RGB_LED_COUNT and RGB_LED_LAYOUT are deliberately absent: they
// are what the variants disagree about.
//
// Target: M5Stack ATOM Lite / ATOM Matrix, both built on the ESP32-PICO-D4
// SiP. Same Xtensa LX6 core and same peripheral register map as the
// WROVER/DevKitC boards, so nothing in the arch layer changes for this
// board — only pin routing and available peripherals differ.
//
// This is a first bring-up manifest: only the UART0 console is wired into
// `TARGET_BUSES`/`TARGET_PERIPHERALS`. The board also exposes a Grove port,
// a button and an RGB LED (consts below), but none of those has a bus/logical
// driver in this tree yet, so they are recorded as plain pin constants
// rather than invented `BusMapping`/`BusDevice` entries — a bus entry this
// crate doesn't back with a real driver would just be a landmine for
// whoever wires one up next.

use hal::bus::*;
use soc_esp32::addr;

pub const TICK_PERIOD_US: u32 = 1000;
pub const DMA_POOL_BYTES: usize = 8192;

/// **Do not use GPIO16 or GPIO17 on this board.**
///
/// This constant used to be `PSRAM_FREE_GPIOS = [16, 17]`, on the reasoning
/// that the ESP32-PICO-D4 has no external PSRAM and so, unlike a WROVER, was
/// free to use them. The reasoning was wrong and the constant was worse than
/// useless: it invited exactly the thing that breaks.
///
/// The PICO-D4 is a SiP with the flash *inside the package*, and GPIO16 and
/// GPIO17 are part of how the die talks to it. Routing a peripheral onto
/// either one kills the running image mid-instruction — the console output
/// garbles halfway through a line and the chip goes silent, with no fault, no
/// panic and no reset. Found the direct way, by doing it.
///
/// Free pins on the Atom's headers are GPIO 19, 22, 23 and 33. GPIO 21 and 25
/// are the IMU; see [`super::m5_atom_matrix`].
pub const RESERVED_GPIOS: [u8; 2] = [16, 17];

// ── Onboard peripherals (not yet bus-backed) ───────────────────────────────
//
// Verified against the M5Stack ATOM Lite pinout published at
// https://docs.m5stack.com/en/core/ATOM%20Lite and corroborated by
// https://www.espboards.dev/esp32/m5stack-atom/ (August 2026): RGB LED on
// GPIO27, programmable button on GPIO39, Grove port I2C on GPIO26 (SDA) /
// GPIO32 (SCL). ATOM Lite and ATOM Matrix share this pinout.

/// Onboard addressable RGB LED (SK6812/WS2812-style, single-wire).
pub const RGB_LED_GPIO: u8 = 27;
//
// How many LEDs hang off that pin, and how they are folded, is the one thing
// the two Atom variants disagree about -- see `m5_atom_lite.rs` and
// `m5_atom_matrix.rs`. A pin without a count is half a fact: an application
// told only the pin drives the first LED and leaves the other 24 dark, which
// looks like a wiring fault.

/// Onboard programmable button.
pub const BUTTON_GPIO: u8 = 39;

/// Grove port, wired as I2C. No `BusMapping` entry yet — add one (bus
/// "i2c0", same base_addr/irq as the WROVER/DevKitC i2c0 peripheral) once a
/// Grove I2C device actually needs it.
pub const GROVE_SDA_GPIO: u8 = 26;
pub const GROVE_SCL_GPIO: u8 = 32;

/// Physical bus drivers to instantiate at boot.
///
/// Only the UART0 console for now — see module docs. Do not add spi3/i2c0
/// entries here by copying esp32_wrover.rs: this board has no onboard
/// BME280/SSD1306, and inventing pins for a bus nothing uses is exactly the
/// kind of copy-paste error the manifest invariant tests in this crate
/// exist to catch.
pub const TARGET_BUSES: &[BusMapping] = &[BusMapping {
    name: "uart0",
    kind: BusKind::Uart,
    base_addr: addr::UART0_BASE,
    irq: addr::IRQ_UART0,
    dma_capable: true,
    dma_pool_bytes: 512,
    config: BusConfig::Uart {
        tx: 1,     // GPIO1 (default TX)
        rx: 3,     // GPIO3 (default RX)
        baud: 115200,
        data_bits: UartDataBits::Bits8,
        parity: UartParity::None,
        stop_bits: UartStopBits::Stop1,
    },
}];

/// Logical device drivers attached to buses.
///
/// Empty: the Atom has no onboard BME280 or SSD1306 (those are WROVER dev
/// board wiring). Copying them here would describe hardware that doesn't
/// exist. An empty list is the honest answer until a real Grove/HY2.0
/// device is attached and its logical driver exists in this tree.
pub const TARGET_DEVICES: &[BusDevice] = &[];

/// Direct peripheral drivers (not bus-attached).
pub const TARGET_PERIPHERALS: &[PeripheralMapping] = &[
    PeripheralMapping { name: "gpio", base_addr: addr::GPIO_BASE, irq: addr::IRQ_GPIO, dma_capable: false, dma_pool_bytes: 0 },
    PeripheralMapping { name: "uart0", base_addr: addr::UART0_BASE, irq: addr::IRQ_UART0, dma_capable: true, dma_pool_bytes: 512 },
];

/// System service tasks.
pub const TARGET_SERVICES: &[ServiceMapping] = &[
    ServiceMapping { name: "devfs", always: true },
    ServiceMapping { name: "procfs", always: false },
    ServiceMapping { name: "debug", always: false },
];
