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

use hal::bus::*;
use soc_esp32::addr;

pub const BOARD_NAME: &str = "ESP32-DevKitC";

/// Radios this board's module physically carries.
///
/// A fact about the hardware, in the same way a pin number is — which is why
/// it lives in the manifest rather than being assumed from the SoC family.
/// Every ESP32 part FlintOS supports today has both, but the ESP32-S2 has no
/// Bluetooth at all and would declare `HAS_BT = false`, so applications that
/// ask for a radio the board has not got fail to build instead of failing to
/// connect.
///
/// The kernel checks these against the `radio-*` features; see
/// `kernel/src/radio.rs`.
pub const HAS_WIFI: bool = true;
pub const HAS_BT: bool = true;


/// A GPIO this board holds at a hard, low-impedance high, or `None`.
///
/// The ADC self-test needs one, and the chip cannot supply it: a pad in analog
/// mode has its digital buffers bypassed, and the internal pull-up is tens of
/// kilohms into the SAR's sampling capacitor -- measured at 4% of full scale
/// rather than 80. So the *board* has to provide the high, which makes it a
/// manifest fact rather than something the test can assume.
///
/// A board without one is not broken; the test skips, and says so.
///
/// A bare DevKitC has nothing on GPIO39 (SENSOR_VN), and GPIO34-39 are
/// input-only with no internal pull at all, so the pin floats. Jumper GPIO39
/// to 3V3 and change this to `Some(39)` to run the test here.
pub const ADC_EXTERNAL_HIGH_GPIO: Option<u8> = None;

/// A GPIO that is electrically free, for on-chip loopback self-tests (TWAI,
/// I2S) that route a peripheral's output and input to the same pad through the
/// matrix. Nothing is connected — the loopback is internal — so the pin only
/// has to be one no other peripheral or the board itself drives.
///
/// A board that declares `None` skips those tests. GPIO21 is unbonded to
/// anything on a bare DevKitC (the I2C convention pins carry nothing by
/// default), clear of the strapping and flash pins.
pub const LOOPBACK_SCRATCH_GPIO: Option<u8> = Some(21);

/// Two more electrically-free pads for loopback self-tests that need more than
/// the single [`LOOPBACK_SCRATCH_GPIO`], as `(a, b)`.
///
/// A bus's `init` insists on distinct pins, so a folded single-pad loopback
/// still needs spare pads to get through `init` before the second signal is
/// folded onto the scratch pad: SPI uses both (a clock pad and a placeholder
/// MISO), UART uses one (a placeholder RX). None carries an external wire —
/// each is only ever driven-and-ignored or only read. GPIO23 and GPIO19 are
/// free on a bare DevKitC — the pair `apps/tests/spidma` has always looped on.
///
/// A board that declares `None` skips those loopback tests.
pub const LOOPBACK_AUX_GPIOS: Option<(u8, u8)> = Some((23, 19));

/// Four electrically-free pads for the on-chip SPI master↔slave loopback, as
/// `[sck, mosi, miso, cs]`, or `None`.
///
/// The self-test drives SPI2 as master and SPI3 as slave and joins their
/// signals through the GPIO matrix with no external wire: master SCK-out →
/// slave SCK-in on the first pad, master MOSI-out → slave MOSI-in on the second,
/// slave MISO-out → master MISO-in on the third, and a master-driven CS →
/// slave CS-in on the fourth. The real CS edge frames each transaction — the
/// slave commits its received data when CS deasserts.
///
/// Separate from [`LOOPBACK_SCRATCH_GPIO`]/[`LOOPBACK_AUX_GPIOS`] so the
/// single-pad folded loopbacks and this two-controller one cannot fight over
/// what a pad means. The pads are the free GPIOs 21/23/19 plus a PSRAM-free 16 —
/// nothing is wired to them and the tests never run at once.
///
/// A board that declares `None` skips the test.
pub const SPI_SLAVE_LOOPBACK_GPIOS: Option<[u8; 4]> = Some([21, 23, 19, 16]);

/// A single electrically-free pad for the PCNT self-test. The test drives it in
/// software and routes it straight back to a PCNT unit's signal input through
/// the matrix, so — like the other loopback pads — nothing external is
/// connected and the pin only has to be one no other peripheral drives. GPIO22
/// is free on a bare DevKitC (the I2C convention's SCL pin, carrying nothing by
/// default) and clear of the strapping and flash pins.
///
/// A board that declares `None` skips the PCNT self-test.
pub const PCNT_LOOPBACK_GPIO: Option<u8> = Some(22);

/// A free touch-capable pad for the touch-sensor self-test, or `None` to skip.
///
/// Only ten pins can sense touch (T0–T9 = GPIO 4/0/2/15/13/12/14/27/32/33), so
/// unlike the other loopback pads this cannot reuse the general free GPIOs. The
/// test does not need a wire — it reads the pad's own parasitic capacitance to
/// prove the controller measures — only a touch-capable pin no other peripheral
/// drives. GPIO27 is T7, is not a strapping or flash pin, and carries nothing on
/// a bare DevKitC.
pub const TOUCH_SELFTEST_GPIO: Option<u8> = Some(27);

/// Three free pads for the MCPWM self-test, as `[pwm_a, pwm_b, fault]`, or
/// `None` to skip. No wire: the driver routes PWM0A/PWM0B out to the first two
/// pads and taps them back through the matrix into PCNT (edge counting) and the
/// MCPWM capture unit (dead-time timing); the fault pad is driven high in
/// software and routed into the MCPWM fault input to prove the hardware
/// shutdown. GPIO 21/23/19 are the free loopback pads, clear of the strapping
/// and flash pins; reused here because the self-tests run one at a time.
pub const MCPWM_SELFTEST_GPIOS: Option<[u8; 3]> = Some([21, 23, 19]);


/// Maximum radio transmit power, in dBm.
///
/// A regulatory and thermal choice, not a chip fact, which is why it is a
/// board manifest entry rather than a constant in the radio crate. It feeds
/// the six transmit-power entries of the PHY's initialisation table
/// (`radio_esp32::phy_init`), clamped per modulation exactly as esp-idf's
/// `LIMIT` macro does.
///
/// 20 dBm is esp-idf's own default, and saturates every band. Lower it for a
/// board that runs hot, has a poor antenna, or ships somewhere with a tighter
/// limit than the part can reach.
pub const PHY_MAX_TX_POWER_DBM: i32 = 20;

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
