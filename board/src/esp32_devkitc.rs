// SPDX-License-Identifier: Apache-2.0

// ESP32-DevKitC / ESP32-WROOM-32 Board Manifest
//
// Target: generic ESP32-DevKitC carrying an ESP32-WROOM-32 module. This is
// the most common bare ESP32 dev board in circulation, and electrically it
// is the ESP32-WROVER manifest (`esp32_wrover.rs`) minus the PSRAM
// constraint: WROOM-32 has no PSRAM, so GPIO16/GPIO17 (which a WROVER
// module reserves internally for the PSRAM's SPI bus) are ordinary free
// GPIOs here. Nothing below currently uses 16/17, so the bus map is
// otherwise identical to the WROVER manifest — same SoC, same default
// IO_MUX pins, same interrupt sources.

use flint_hal::bus::*;
use flint_soc_esp32::addr;

pub const BOARD_NAME: &str = "ESP32-DevKitC";
pub const TICK_PERIOD_US: u32 = 1000;
pub const DMA_POOL_BYTES: usize = 8192;

/// GPIO16/17 are free on WROOM-32 (no onboard PSRAM to reserve them for),
/// unlike WROVER. Not wired to anything in this manifest; listed for
/// boards/drivers that want to make use of them.
pub const PSRAM_FREE_GPIOS: [u8; 2] = [16, 17];

/// Physical bus drivers to instantiate at boot.
pub const TARGET_BUSES: &[BusMapping] = &[
    BusMapping {
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
    },
    BusMapping {
        // VSPI (SPI3). GPIO 23/19/18 are VSPI's IO_MUX-native pins, matching
        // the WROVER manifest's routing (see esp32_wrover.rs for why SPI2's
        // base address must not be paired with these pins).
        name: "spi3",
        kind: BusKind::Spi,
        base_addr: addr::SPI3_BASE,
        irq: addr::IRQ_SPI3,
        dma_capable: true,
        dma_pool_bytes: 2048,
        config: BusConfig::Spi {
            mosi: 23,
            miso: 19,
            sck: 18,
            max_speed: BusSpeed::MHz(40),
            mode: SpiMode::Mode0,
        },
    },
    BusMapping {
        name: "i2c0",
        kind: BusKind::I2c,
        base_addr: addr::I2C0_BASE,
        irq: addr::IRQ_I2C0,
        dma_capable: false,
        dma_pool_bytes: 0,
        config: BusConfig::I2c {
            sda: 21,
            scl: 22,
            speed: BusSpeed::Fast400k,
        },
    },
];

/// Logical device drivers attached to buses.
///
/// A bare DevKitC has no onboard sensors/displays either — these entries
/// describe the same external BME280/SSD1306 breakout wiring assumed by
/// the WROVER manifest, kept here for parity. Remove them if your board
/// isn't wired up that way.
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
