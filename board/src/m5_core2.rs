// SPDX-License-Identifier: Apache-2.0

// M5Stack Core2 Board Manifest
//
// Target: M5Stack Core2 (ESP32-D0WDQ6-V3, 16 MB flash, 8 MB PSRAM). Unlike a
// bare WROOM the Core2 is a fully-populated board — AXP192 PMIC, MPU6886 IMU,
// ILI9342C LCD, FT6336U touch, all on shared internal buses behind a power
// gate — so most of its pins are *not* free and this manifest declares no
// self-test pads. It carries the console UART, the internal I2C bus, and the
// AXP192 PMIC with the rails it brings up at boot (#135). The remaining devices
// come later (see #136 for the IMU/LCD/touch showcase).
//
// PSRAM: the Core2 carries 8 MB, which would reserve GPIO16/17 for its SPI
// bus, but this tree never initialises PSRAM on any board, so 16/17 are left
// unwired rather than reserved. Nothing here touches them.
//
// FLASH: the 16 MB part reports JEDEC vendor 0x20 (XMC/Micron), which
// `spi1::unlock` does not recognise, so it refuses to clear the block-protect
// bit rather than risk a wrong QE write — verified on hardware, no brick
// (#134, closed). Flash writes are therefore unavailable here until vendor 0x20
// is supported (#137); this manifest declares no flash-touching device.

use hal::bus::*;
use soc_esp32::{addr, I2cCtrl, I2cPort, SpiCtrl, SpiPort};

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
/// The DMA pool's size is set by the build's memory map (`tools/build/src/map.rs`,
/// radio-keyed against the ROM bound), *not* by this constant, and the runtime
/// reads the real size from the linker symbols the map emits. This value is a
/// manifest fact only — it mirrors the Wi-Fi-only pool the map places (12 KiB
/// on this board, #140). The display's DMA chunk follows the pool at runtime
/// rather than a fixed size, so it uses whatever the map leaves it.
pub const DMA_POOL_BYTES: usize = 12288;

/// The LCD's SPI bus and control pins. ILI9342C over SPI3 (VSPI): MOSI 23 and
/// SCK 18 are VSPI-native pads; MISO 38 is unused by the write-only panel and
/// matrix-routed; CS 5 and D/C 15 are driven as plain GPIOs by the display
/// transport (not the SPI controller's hardware CS), so a windowed fill holds CS
/// across the whole transaction. SPI mode 0.
///
/// `max_speed` is 40 MHz — hardware-verified on a Core2 (clean animation; see
/// #140), and the fastest clean rate this panel can take. The SPI clock is an
/// integer divide of the 80 MHz APB, so the only rates available are 80 (÷1),
/// 40 (÷2), 26.7 (÷3)…; there is nothing clean between 40 and 80. 80 MHz was
/// tried on hardware: the driver clocks the pixels out (IO_MUX-native pads,
/// MOSI 23 / SCK 18), but the panel does not latch at that rate and the screen
/// stays blank; 60 MHz has no integer divisor and the fallback ran at ~2 fps.
/// So 40 is both the ceiling and the practical maximum: an app may run slower
/// via [`crate::display_interface_at`], never faster.
const LCD_MOSI_GPIO: u8 = 23;
const LCD_MISO_GPIO: u8 = 38;
const LCD_SCK_GPIO: u8 = 18;
pub const LCD_CS_GPIO: u8 = 5;
pub const LCD_DC_GPIO: u8 = 15;
const LCD_PORT: SpiPort = SpiPort {
    ctrl: SpiCtrl::Spi3,
    cfg: SpiConfig {
        mosi: LCD_MOSI_GPIO,
        miso: LCD_MISO_GPIO,
        sck: LCD_SCK_GPIO,
        max_speed: BusSpeed::MHz(40),
        mode: SpiMode::Mode0,
    },
};

/// The Core2's internal I2C bus: SDA GPIO21, SCL GPIO22. Shared by the AXP192
/// PMIC (0x34), the MPU6886 IMU (0x68) and the FT6336U touch (0x38). Fast-mode
/// 400 kHz. The PMIC and (later, #136) the IMU must both go through one
/// controller — see [`crate::pmic_bus`].
const INTERNAL_I2C_SDA: u8 = 21;
const INTERNAL_I2C_SCL: u8 = 22;
const INTERNAL_I2C_PORT: I2cPort = I2cPort {
    ctrl: I2cCtrl::I2c0,
    cfg: I2cConfig { sda: INTERNAL_I2C_SDA, scl: INTERNAL_I2C_SCL, speed: BusSpeed::Fast400k },
};

/// Physical bus drivers to instantiate at boot: the console UART and the
/// internal I2C bus the PMIC lives on.
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
    BusMapping {
        name: "i2c0",
        kind: BusKind::I2c,
        base_addr: addr::I2C0_BASE,
        irq: addr::IRQ_I2C0,
        dma_capable: false,
        dma_pool_bytes: 0,
        config: BusConfig::i2c(INTERNAL_I2C_SDA, INTERNAL_I2C_SCL, BusSpeed::Fast400k),
    },
    BusMapping {
        name: "spi3",
        kind: BusKind::Spi,
        base_addr: addr::SPI3_BASE,
        irq: addr::IRQ_SPI3,
        dma_capable: true,
        dma_pool_bytes: 8192,
        config: BusConfig::spi_mode0(LCD_MOSI_GPIO, LCD_MISO_GPIO, LCD_SCK_GPIO, BusSpeed::MHz(40)),
    },
];

/// The AXP192 PMIC and the rails it brings up at boot.
///
/// Rail assignments are the M5Core2's (from the M5 schematic / docs), *not*
/// AXP192 defaults: DCDC1 is the ESP32's own 3.3 V system rail — **deliberately
/// absent from the list**, as `power_init` refuses to switch it. Order matters:
/// LDO2 (peripheral 3.3 V — SD card and LCD logic) comes up before DCDC3 (the
/// 2.8 V LCD backlight) that sits on top of it. LDO3 (vibration motor) is left
/// as the bootloader had it.
pub const PMIC_RAILS: &[crate::RailSetup] = &[
    crate::RailSetup { rail: axp192::Rail::Ldo2, millivolts: 3300 },
    crate::RailSetup { rail: axp192::Rail::Dcdc3, millivolts: 2800 },
];

/// The onboard IMU's I2C address and pins, exposed like the Atom Matrix's for
/// the `imu` app that logs them. M5 shipped the Core2 with an MPU6886; the app
/// probes for that and a BMI270 (both answer here) and drives whichever replied.
/// The pins are the internal bus's — the IMU and PMIC share it.
pub const IMU_I2C_ADDR: u8 = 0x68;
pub const IMU_SDA_GPIO: u8 = INTERNAL_I2C_SDA;
pub const IMU_SCL_GPIO: u8 = INTERNAL_I2C_SCL;

/// The FT6336U capacitive-touch controller's I2C address, on the same internal
/// bus. The touch area is taller than the 320×240 screen — the three capacitive
/// buttons below it — so reported y runs past 240.
pub const TOUCH_I2C_ADDR: u8 = 0x38;

/// Logical devices on the internal I2C bus. The IMU is powered off LDO2, which
/// `power_init` brings up before the app runs, so it is live by the time the
/// app opens the bus. The display and touch controller come later (#136).
pub const TARGET_DEVICES: &[BusDevice] = &[
    BusDevice {
        name: "imu",
        logical_driver: "mpu6886",
        bus: "i2c0",
        cs_pin: None,
        bus_speed: BusSpeed::Fast400k,
    },
    BusDevice {
        name: "touch",
        logical_driver: "ft6336u",
        bus: "i2c0",
        cs_pin: None,
        bus_speed: BusSpeed::Fast400k,
    },
];

/// Direct peripheral drivers (not bus-attached). UART0 is not repeated here —
/// it is a bus in [`TARGET_BUSES`].
pub const TARGET_PERIPHERALS: &[PeripheralMapping] = &[
    PeripheralMapping { name: "gpio", base_addr: addr::GPIO_BASE, irq: addr::IRQ_GPIO, dma_capable: false, dma_pool_bytes: 0 },
];

/// This board as one value; see [`crate::Board`].
///
/// The IMU and the PMIC share the internal I2C0 bus — `imu_bus()` and
/// `pmic_bus()` hand back the same controller. The Core2 has no addressable LED
/// and no free self-test pads.
pub const BOARD: crate::Board = crate::Board {
    name: BOARD_NAME,
    imu: Some(crate::I2cAttachment { port: INTERNAL_I2C_PORT, addr: IMU_I2C_ADDR }),
    pmic: Some(crate::PmicAttachment {
        port: INTERNAL_I2C_PORT,
        addr: axp192::ADDR,
        rails: PMIC_RAILS,
    }),
    touch: Some(crate::I2cAttachment { port: INTERNAL_I2C_PORT, addr: TOUCH_I2C_ADDR }),
    display: Some(crate::DisplayAttachment { port: LCD_PORT, dc: LCD_DC_GPIO, cs: LCD_CS_GPIO }),
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
