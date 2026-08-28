// SPDX-License-Identifier: Apache-2.0

//! Bus, device and peripheral tables shared by the ESP32-WROOM-32 boards.
//!
//! The ESP32-DevKitC (WROOM-32 module) and the ESP32-WROVER are electrically
//! the same part for these tables — same SoC, same default IO_MUX pins, same
//! interrupt sources; the WROVER only reserves GPIO16/17 for its PSRAM, which
//! nothing here uses. Their `TARGET_BUSES`/`TARGET_DEVICES`/`TARGET_PERIPHERALS`
//! were body-identical and had already drifted by a comment, so a corrected bus
//! mapping in one would not reach the other. They live here once and both
//! manifests re-export them; only the board-specific pads and radios stay per
//! file.

use hal::bus::*;
use soc_esp32::addr;

pub const TARGET_BUSES: &[BusMapping] = &[
    BusMapping {
        name: "uart0",
        kind: BusKind::Uart,
        base_addr: addr::UART0_BASE,
        irq: addr::IRQ_UART0,
        dma_capable: true,
        dma_pool_bytes: 512,
        config: BusConfig::uart_8n1(crate::ESP32_CONSOLE.tx, crate::ESP32_CONSOLE.rx, crate::ESP32_CONSOLE.baud),
    },
    BusMapping {
        // VSPI (SPI3). GPIO 23/19/18 are VSPI's IO_MUX-native pins; pairing
        // them with SPI2's base address at 0x3FF64000 describes a bus that
        // cannot be routed without the GPIO matrix, so SPI3's base is used.
        name: "spi3",
        kind: BusKind::Spi,
        base_addr: addr::SPI3_BASE,
        irq: addr::IRQ_SPI3,
        dma_capable: true,
        dma_pool_bytes: 2048,
        config: BusConfig::spi_mode0(23, 19, 18, BusSpeed::MHz(40)),
    },
    BusMapping {
        name: "i2c0",
        kind: BusKind::I2c,
        base_addr: addr::I2C0_BASE,
        irq: addr::IRQ_I2C0,
        dma_capable: false,
        dma_pool_bytes: 0,
        config: BusConfig::i2c(21, 22, BusSpeed::Fast400k),
    },
];

pub const TARGET_DEVICES: &[BusDevice] = &[
    BusDevice {
        name: "temp_sensor",
        logical_driver: "bme280",
        bus: "spi3",
        cs_pin: Some(15),
        bus_speed: BusSpeed::MHz(4),
    },
    BusDevice {
        name: "display",
        logical_driver: "ssd1306",
        bus: "i2c0",
        cs_pin: None,
        bus_speed: BusSpeed::Fast400k,
    },
];

pub const TARGET_PERIPHERALS: &[PeripheralMapping] = &[
    PeripheralMapping { name: "gpio", base_addr: addr::GPIO_BASE, irq: addr::IRQ_GPIO, dma_capable: false, dma_pool_bytes: 0 },
];

/// The `nvs` key/value partition, `(offset, len)`, as espflash's default table
/// places it — quoted from a running board's boot log:
/// `nvs   WiFi data  01 02 00009000 00006000`. A board that flashes a custom
/// partition layout overrides this in its own manifest.
pub const NVS_PARTITION: (u32, u32) = (0x9000, 0x6000);
