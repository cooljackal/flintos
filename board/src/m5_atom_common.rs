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
/// The Atom's button sits on GPIO39, pulled up by an external resistor, and
/// GPIO39 is ADC1 channel 3.
pub const ADC_EXTERNAL_HIGH_GPIO: Option<u8> = Some(39);

/// A GPIO free for on-chip loopback self-tests (TWAI, I2S). `None` skips them.
/// The Atom modules bond almost every pin (LED, IR, button, Grove, IMU), so no
/// pin is confidently free for a scratch loopback here.
pub const LOOPBACK_SCRATCH_GPIO: Option<u8> = None;

/// Spare `(a, b)` aux pads for single-pad loopback self-tests. `None` skips
/// them — see the DevKitC manifest for what these are for.
pub const LOOPBACK_AUX_GPIOS: Option<(u8, u8)> = None;

/// Three free pads `[sck, mosi, miso]` for the SPI master↔slave loopback, or
/// `None`. See the DevKitC manifest for what this test does. Unset: the Atom
/// modules bond almost every pin, so no trio is confidently free here.
pub const SPI_SLAVE_LOOPBACK_GPIOS: Option<[u8; 4]> = None;
/// A free pad for the PCNT self-test. `None` skips it — the Atom modules bond
/// almost every pin, so none is confidently free here.
pub const PCNT_LOOPBACK_GPIO: Option<u8> = None;

/// No touch-sensor self-test on this board (see the DevKitC manifest).
pub const TOUCH_SELFTEST_GPIO: Option<u8> = None;

/// No MCPWM self-test on this board (see the DevKitC manifest).
pub const MCPWM_SELFTEST_GPIOS: Option<[u8; 3]> = None;


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
/// The DMA pool's size is set by the build's memory map (`tools/build/src/map.rs`,
/// radio-keyed against the ROM bound), *not* by this constant, and the runtime
/// reads the real size from the linker symbols the map emits. This value is a
/// manifest fact only: it mirrors the Wi-Fi-only pool the map places (12 KiB; a
/// `radio-bt` build gets 8 KiB, the ROM bound being tighter once the controller
/// takes the bottom of DRAM). See #140.
pub const DMA_POOL_BYTES: usize = 12288;

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
    config: BusConfig::uart_8n1(1, 3, 115200),
}];

/// Logical device drivers attached to buses.
///
/// Empty: the Atom has no onboard BME280 or SSD1306 (those are WROVER dev
/// board wiring). Copying them here would describe hardware that doesn't
/// exist. An empty list is the honest answer until a real Grove/HY2.0
/// device is attached and its logical driver exists in this tree.
pub const TARGET_DEVICES: &[BusDevice] = &[];

/// Direct peripheral drivers (not bus-attached).
///
/// UART0 is not repeated here: it is a bus in [`TARGET_BUSES`], and listing it
/// again as a peripheral described one controller twice.
pub const TARGET_PERIPHERALS: &[PeripheralMapping] = &[
    PeripheralMapping { name: "gpio", base_addr: addr::GPIO_BASE, irq: addr::IRQ_GPIO, dma_capable: false, dma_pool_bytes: 0 },
];

/// The free pads and console shared by both Atom variants. The variants add the
/// `imu` and `rgb_led` they disagree about; see their `BOARD` consts. The Atom
/// modules bond almost every pin, so no loopback pad is confidently free.
pub const SELFTEST_PADS: crate::SelftestPads = crate::SelftestPads {
    scratch: LOOPBACK_SCRATCH_GPIO,
    aux: LOOPBACK_AUX_GPIOS,
    spi_slave: SPI_SLAVE_LOOPBACK_GPIOS,
    pcnt: PCNT_LOOPBACK_GPIO,
    touch: TOUCH_SELFTEST_GPIO,
    mcpwm: MCPWM_SELFTEST_GPIOS,
    adc_external_high: ADC_EXTERNAL_HIGH_GPIO,
};

/// The console pins shared by both Atom variants.
pub const CONSOLE: crate::ConsolePins = crate::ConsolePins { tx: 1, rx: 3, baud: 115_200 };

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
