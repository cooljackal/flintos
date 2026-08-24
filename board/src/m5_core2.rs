// SPDX-License-Identifier: Apache-2.0

// M5Stack Core2 Board Manifest
//
// Target: M5Stack Core2 (ESP32-D0WDQ6-V3, 16 MB flash, 8 MB PSRAM). Unlike a
// bare WROOM the Core2 is a fully-populated board — AXP192 PMIC, MPU6886 IMU,
// ILI9342C LCD, FT6336U touch, all on shared internal buses behind a power
// gate — so most of its pins are *not* free and this manifest declares no
// self-test pads. It is deliberately minimal: UART0 console and GPIO only,
// enough to boot and print. The devices and their power ordering come later
// (see #133 for this phase, #135 for the AXP192 rail-ordering, #136 for the
// IMU/LCD/touch showcase).
//
// PSRAM: the Core2 carries 8 MB, which would reserve GPIO16/17 for its SPI
// bus, but this tree never initialises PSRAM on any board, so 16/17 are left
// unwired rather than reserved. Nothing here touches them.
//
// FLASH BRICKING HAZARD: the 16 MB flash part is the first non-GigaDevice
// chip this tree meets, and `spi1::unlock` writes the status register with no
// manufacturer check (QE at bit 9 — wrong for Macronix/ISSI). Do not run the
// flash-erase self-test on this board until that path is gated on the JEDEC
// ID (#134). This manifest declares no flash-touching device for that reason.

use hal::bus::*;
use soc_esp32::addr;

pub const BOARD_NAME: &str = "M5Stack Core2";

/// Radios this board's module physically carries. The Core2's ESP32-D0WDQ6-V3
/// has both Wi-Fi and Bluetooth, same as the WROOM. See the DevKitC manifest
/// for why this is a board fact rather than a SoC assumption.
pub const HAS_WIFI: bool = true;
pub const HAS_BT: bool = true;

/// No free self-test pads on the Core2: every pad is spoken for by the LCD,
/// touch, IMU, PMIC or SD slot, so the on-chip loopback self-tests (ADC, SPI
/// master↔slave, PCNT, touch, MCPWM) skip and say so. Revisit once the device
/// wiring is confirmed and any genuinely-spare pads are known.
pub const ADC_EXTERNAL_HIGH_GPIO: Option<u8> = None;
pub const LOOPBACK_SCRATCH_GPIO: Option<u8> = None;
pub const LOOPBACK_AUX_GPIOS: Option<(u8, u8)> = None;
pub const SPI_SLAVE_LOOPBACK_GPIOS: Option<[u8; 4]> = None;
pub const PCNT_LOOPBACK_GPIO: Option<u8> = None;
pub const TOUCH_SELFTEST_GPIO: Option<u8> = None;
pub const MCPWM_SELFTEST_GPIOS: Option<[u8; 3]> = None;

/// Maximum radio transmit power, in dBm. esp-idf's default; see the DevKitC
/// manifest for the regulatory/thermal rationale.
pub const PHY_MAX_TX_POWER_DBM: i32 = 20;

pub const TICK_PERIOD_US: u32 = 1000;
pub const DMA_POOL_BYTES: usize = 8192;

/// Physical bus drivers to instantiate at boot.
///
/// Phase 1 declares only the console UART. The Core2's internal I2C bus
/// (AXP192 at 0x34, MPU6886 at 0x68, FT6336U at 0x38, on GPIO21/22) waits for
/// the AXP192 driver, which owns the rail those devices sit behind (#135).
pub const TARGET_BUSES: &[BusMapping] = &[
    BusMapping {
        name: "uart0",
        kind: BusKind::Uart,
        base_addr: addr::UART0_BASE,
        irq: addr::IRQ_UART0,
        dma_capable: true,
        dma_pool_bytes: 512,
        config: BusConfig::uart_8n1(1, 3, 115200),
    },
];

/// No logical devices yet — the Core2's sensors and display sit behind the
/// AXP192 power gate (#135/#136).
pub const TARGET_DEVICES: &[BusDevice] = &[];

/// Direct peripheral drivers (not bus-attached). UART0 is not repeated here —
/// it is a bus in [`TARGET_BUSES`].
pub const TARGET_PERIPHERALS: &[PeripheralMapping] = &[
    PeripheralMapping { name: "gpio", base_addr: addr::GPIO_BASE, irq: addr::IRQ_GPIO, dma_capable: false, dma_pool_bytes: 0 },
];

/// This board as one value; see [`crate::Board`].
///
/// The IMU is present on hardware (MPU6886 at 0x68) but declared `None` until
/// the AXP192 rail that powers the internal I2C bus is brought up (#136); the
/// Core2 has no addressable LED and no free self-test pads.
pub const BOARD: crate::Board = crate::Board {
    name: BOARD_NAME,
    imu: None,
    rgb_led: None,
    selftest: crate::SelftestPads {
        scratch: LOOPBACK_SCRATCH_GPIO,
        aux: LOOPBACK_AUX_GPIOS,
        spi_slave: SPI_SLAVE_LOOPBACK_GPIOS,
        pcnt: PCNT_LOOPBACK_GPIO,
        touch: TOUCH_SELFTEST_GPIO,
        mcpwm: MCPWM_SELFTEST_GPIOS,
        adc_external_high: ADC_EXTERNAL_HIGH_GPIO,
    },
    console: crate::ConsolePins { tx: 1, rx: 3, baud: 115_200 },
};
