// SPDX-License-Identifier: Apache-2.0

//! Board manifest for FlintOS.
//!
//! Each supported board is a submodule that exports:
//! - `BOARD` — the board as one [`Board`] value, the facts an application asks
//!   about gathered with `Option` fields (see [`Board`])
//! - `TARGET_BUSES` — physical bus definitions
//! - `TARGET_DEVICES` — logical device attachments
//! - `TARGET_PERIPHERALS` — direct peripheral mappings (bus controllers are not
//!   repeated here)
//!
//! ## What belongs in a manifest
//!
//! Every fact about the board that an application would otherwise have to
//! look up in a datasheet: pins, base addresses, IRQ numbers, and the shape of
//! anything attached. A pin without the count of what is on it is half a fact
//! — `RGB_LED_GPIO` alone let an application drive one LED of a 25-LED panel
//! and look correct while 24 stayed dark.
//!
//! ## Board selection
//!
//! The active board is chosen at compile time via Cargo features, one per
//! supported board. Exactly one must be enabled — the guards below turn
//! "zero selected" and "more than one selected" into build failures
//! instead of a silently wrong manifest, which on real hardware shows up as a
//! very confusing bring-up bug (wrong pins, wrong IRQ, etc).
//!
//! Downstream crates (namely `kernel`) never name a board module directly;
//! they use `board::active`, which this crate re-exports to whichever board
//! module was selected.
//!
//! ```text
//! cargo build -p kernel --no-default-features --features board-m5-atom-matrix
//! cargo build -p kernel --no-default-features --features board-esp32-devkitc
//! cargo build -p kernel   # default: board-esp32-wrover
//! ```
//!
//! Adding a new board: add a `board-<name>` feature in `Cargo.toml`, a
//! `#[cfg(feature = "board-<name>")] pub mod <name>;` line below, one line
//! in the `SELECTED` count, one line in the "no board selected" message, and
//! an arm in the `active` re-export block.

#![no_std]

#[cfg(feature = "board-esp32-wrover")]
pub mod esp32_wrover;

#[cfg(feature = "board-esp32-devkitc")]
pub mod esp32_devkitc;

#[cfg(feature = "board-m5-atom-lite")]
pub mod m5_atom_lite;

#[cfg(feature = "board-m5-atom-matrix")]
pub mod m5_atom_matrix;

#[cfg(feature = "board-wio-rp2040-mini")]
pub mod wio_rp2040_mini;

/// Pin map shared by both Atom variants. Not selectable on its own: it declares
/// no LED count, because that is the only thing the two disagree about.
#[cfg(any(feature = "board-m5-atom-lite", feature = "board-m5-atom-matrix"))]
mod m5_atom_common;

// ── Exactly-one-board enforcement ───────────────────────────────────────────

// `board-m5-atom` is excluded here because it has its own message below, and
// two compile errors for one mistake buries the one that says what to do.
#[cfg(all(
    not(any(
        feature = "board-esp32-wrover",
        feature = "board-esp32-devkitc",
        feature = "board-m5-atom-lite",
        feature = "board-m5-atom-matrix",
        feature = "board-wio-rp2040-mini",
    )),
    not(feature = "board-m5-atom")
))]
compile_error!(
    "board: no board selected, and there is no default.

     A board manifest is the pin map, the bus map and the IRQ numbers. A
     default would mean flashing one of those without having chosen it, so
     there isn't one -- name the board:

     	make flash BOARD=board-esp32-devkitc
     	cargo build -p kernel --features board-esp32-devkitc

     	board-esp32-devkitc     ESP32-DevKitC / WROOM-32   (verified on hardware)
     	board-m5-atom-matrix    M5Stack Atom Matrix        (verified on hardware)
     	board-m5-atom-lite      M5Stack Atom Lite          (verified on hardware)
     	board-esp32-wrover      ESP32-WROVER               (never flashed)
     	board-wio-rp2040-mini   Seeed Wio RP2040 Mini      (connected; first light pending)"
);

// How many `board-*` features are on. `cfg!()` is a const bool, so the count
// is a const and the assert fails at compile time with the message below.
// A new board is one line here: `+ cfg!(feature = "board-<name>") as usize`.
// (The zero case has its own `compile_error!` above so it can list the boards.)
const SELECTED: usize = cfg!(feature = "board-esp32-wrover") as usize
    + cfg!(feature = "board-esp32-devkitc") as usize
    + cfg!(feature = "board-m5-atom-lite") as usize
    + cfg!(feature = "board-m5-atom-matrix") as usize
    + cfg!(feature = "board-wio-rp2040-mini") as usize;

const _: () = assert!(
    SELECTED <= 1,
    "board: more than one `board-*` feature is enabled. A build with two \
     board manifests merged in is not a real board -- it silently produces \
     the wrong pin/IRQ/bus map. Build with \
     `--no-default-features --features <one-board>`, one of: \
     board-esp32-devkitc, board-m5-atom-matrix, board-m5-atom-lite, \
     board-esp32-wrover, board-wio-rp2040-mini."
);

// The name the Atom shipped under before the Lite and the Matrix were told
// apart. Kept as a feature purely so this message can be printed: dropping it
// outright leaves cargo saying "does not contain this feature", which is true
// and says nothing about which of the two to pick.
#[cfg(feature = "board-m5-atom")]
compile_error!(
    "board: `board-m5-atom` no longer names a board. The Atom Lite has one LED      and the Atom Matrix has a 5×5 panel on the same pin, and a manifest that      cannot tell them apart lets an application light one pixel of a panel and      look correct.
     
     	board-m5-atom-lite     one SK6812
     	board-m5-atom-matrix   5×5 panel, 25 LEDs
"
);

// ── Active board re-export ──────────────────────────────────────────────────
//
// Plain `#[cfg(feature)]` arms: `SELECTED` above guarantees at most one is
// on, so no arm needs to exclude the others.

#[cfg(feature = "board-esp32-wrover")]
pub use esp32_wrover as active;

#[cfg(feature = "board-esp32-devkitc")]
pub use esp32_devkitc as active;

#[cfg(feature = "board-m5-atom-lite")]
pub use m5_atom_lite as active;

#[cfg(feature = "board-m5-atom-matrix")]
pub use m5_atom_matrix as active;

#[cfg(feature = "board-wio-rp2040-mini")]
pub use wio_rp2040_mini as active;

// ── The board as one value ──────────────────────────────────────────────────
//
// One `pub const BOARD: Board` per board module gathers the manifest facts an
// application asks about into a single value whose fields are `Option`s. An app
// then guards on the *fact* -- `board::BOARD.imu.is_some()` -- rather than on a
// board name or a feature it no longer declares, and a board can be checked for
// completeness in one place. The loose `TARGET_*`/`*_GPIO` consts stay for the
// kernel self-tests and drivers that already read them; `BOARD` is the value an
// application reaches for.

/// A board-owned addressable-LED strip or panel.
///
/// `layout` is `None` for a single LED (no geometry to fold) and `Some` for a
/// panel — the same fact the `RGB_LED_LAYOUT` const carries, gathered here with
/// the pin and the count so an application takes all three as a unit.
#[derive(Copy, Clone, Debug)]
pub struct RgbLed {
    pub gpio: u8,
    pub count: usize,
    pub layout: Option<led_matrix::Layout>,
}

/// The board's console pins and baud.
///
/// Every board FlintOS runs puts its console on UART0, so the controller is
/// implied; only the pins and baud are a board fact. Held as plain numbers so
/// the one struct serves both SoC families — each board's `console_init` builds
/// the SoC-specific port from these.
#[derive(Copy, Clone, Debug)]
pub struct ConsolePins {
    pub tx: u8,
    pub rx: u8,
    pub baud: u32,
}

/// Electrically-free pads a board offers the on-chip loopback self-tests and
/// the bus/stream porting examples.
///
/// Each mirrors a loose `*_GPIO` const; gathered here so `BOARD.selftest` is one
/// value. `None` on any field means "no free pad for that test on this board",
/// and the test or example skips and says so. Plain pin numbers, so the struct
/// is arch-neutral.
#[derive(Copy, Clone, Debug, Default)]
pub struct SelftestPads {
    /// A single free pad for the folded loopbacks (TWAI, I2S) and the
    /// bus/stream examples' data line.
    pub scratch: Option<u8>,
    /// Two more free pads a folded single-pad loopback needs to clear `init`'s
    /// distinct-pin check.
    pub aux: Option<(u8, u8)>,
    /// Four free pads `[sck, mosi, miso, cs]` for the SPI master↔slave loopback.
    pub spi_slave: Option<[u8; 4]>,
    /// A free pad for the PCNT self-test.
    pub pcnt: Option<u8>,
    /// A free touch-capable pad for the touch-sensor self-test.
    pub touch: Option<u8>,
    /// Three free pads `[pwm_a, pwm_b, fault]` for the MCPWM self-test.
    pub mcpwm: Option<[u8; 3]>,
    /// A pad the board holds at a hard high for the ADC self-test.
    pub adc_external_high: Option<u8>,
}

/// An I2C device wired onto the board: which controller and pins bring it up
/// (an [`I2cPort`](soc_esp32::I2cPort), so "which controller" is no longer
/// half-declared), and the 7-bit address it answers at.
#[cfg(any(
    feature = "board-esp32-wrover",
    feature = "board-esp32-devkitc",
    feature = "board-m5-atom-lite",
    feature = "board-m5-atom-matrix",
))]
#[derive(Copy, Clone, Debug)]
pub struct I2cAttachment {
    pub port: soc_esp32::I2cPort,
    pub addr: u8,
}

/// Everything an application asks a board manifest about, as one value.
#[derive(Copy, Clone, Debug)]
pub struct Board {
    /// Human-readable board name.
    pub name: &'static str,
    /// The onboard IMU, if the board has one on a private I2C bus.
    #[cfg(any(
        feature = "board-esp32-wrover",
        feature = "board-esp32-devkitc",
        feature = "board-m5-atom-lite",
        feature = "board-m5-atom-matrix",
    ))]
    pub imu: Option<I2cAttachment>,
    /// The onboard addressable RGB LED or panel, if any.
    pub rgb_led: Option<RgbLed>,
    /// Free pads for the self-tests and porting examples.
    pub selftest: SelftestPads,
    /// The console pins.
    pub console: ConsolePins,
}

/// The active board, as one value. Re-exported from the selected board module.
#[cfg(any(
    feature = "board-esp32-wrover",
    feature = "board-esp32-devkitc",
    feature = "board-m5-atom-lite",
    feature = "board-m5-atom-matrix",
    feature = "board-wio-rp2040-mini",
))]
pub use active::BOARD;

// ── Device accessors ─────────────────────────────────────────────────────────
//
// The board owns its devices now. Each accessor opens a controller once (the
// `open` claims it, so a second call would return `BusError::Busy`), wraps it
// in its Layer-2 bus, and caches the whole stack in an `api::sync::Once` so
// later calls hand back the same `&'static`. An application asks the board for
// a ready bus instead of open-coding `new(base) + init + static mut`. Gated on
// `esp32-drivers`: only the ESP32 boards pull the drivers these construct.

/// The onboard IMU's I2C bus: opens I2C0 on the board's private IMU pins and
/// returns the shared controller. `Error::Other` if this board declares no IMU
/// (`BOARD.imu` is `None`).
///
/// The returned [`I2cController`](i2c_bus::I2cController) owns its
/// [`Esp32I2c`](esp32_i2c::Esp32I2c) by value; take a device handle with
/// `.device(addr)` for the address to talk to.
#[cfg(feature = "esp32-drivers")]
pub fn imu_bus() -> hal::Result<&'static i2c_bus::I2cController<esp32_i2c::Esp32I2c>> {
    static IMU_BUS: api::Once<i2c_bus::I2cController<esp32_i2c::Esp32I2c>> = api::Once::new();
    if let Some(bus) = IMU_BUS.get() {
        return Ok(bus);
    }
    let imu = active::BOARD
        .imu
        .ok_or(hal::Error::Other("this board declares no onboard IMU"))?;
    IMU_BUS.get_or_try_init(|| {
        Ok(i2c_bus::I2cController::new(esp32_i2c::Esp32I2c::open(&imu.port)?))
    })
}

/// The free pads for the loopback bus/stream porting examples, or `None` if
/// this board declares none.
///
/// `scratch` is the single data pad the fold routes onto; `aux` are the two
/// spare pads `init` needs to see distinct pins before the fold. See the
/// DevKitC manifest for the electrical story.
#[cfg(feature = "esp32-drivers")]
pub fn loopback_pads() -> Option<LoopbackPads> {
    Some(LoopbackPads {
        scratch: active::BOARD.selftest.scratch?,
        aux: active::BOARD.selftest.aux?,
    })
}

/// The two spare pads a folded single-pad loopback needs, plus the pad the fold
/// routes onto. See [`loopback_pads`].
#[cfg(feature = "esp32-drivers")]
#[derive(Copy, Clone, Debug)]
pub struct LoopbackPads {
    /// The single data pad MOSI/MISO (SPI) or TX (UART) folds onto.
    pub scratch: u8,
    /// `(a, b)`: SCK and a placeholder MISO for SPI, or a placeholder RX for
    /// UART — the spare pins `init` wants distinct before the fold.
    pub aux: (u8, u8),
}

/// A SPI bus opened on the board's loopback pads (SPI2, mode 0, 4 MHz), for the
/// `spitxrx` porting example. `Error::Other` if this board declares no loopback
/// pads.
///
/// The bus is brought up on three distinct pads; the caller then folds MISO
/// onto the MOSI pad to make the on-chip loopback (`init` refuses two signals
/// on one pad, so the fold cannot be part of bring-up).
#[cfg(feature = "esp32-drivers")]
pub fn loopback_spi() -> hal::Result<&'static spi_bus::SpiBus<esp32_spi::Esp32Spi>> {
    use hal::bus::{SpiConfig, SpiMode};
    use soc_esp32::{SpiCtrl, SpiPort};

    static LOOPBACK_SPI: api::Once<spi_bus::SpiBus<esp32_spi::Esp32Spi>> = api::Once::new();
    if let Some(bus) = LOOPBACK_SPI.get() {
        return Ok(bus);
    }
    let pads = loopback_pads().ok_or(hal::Error::Other("this board declares no loopback pads"))?;
    // mosi = scratch, sck = aux.0, miso = aux.1 (a placeholder pad until the
    // caller folds MISO onto the scratch pad).
    let port = SpiPort {
        ctrl: SpiCtrl::Spi2,
        cfg: SpiConfig {
            mosi: pads.scratch,
            miso: pads.aux.1,
            sck: pads.aux.0,
            max_speed: hal::bus::BusSpeed::MHz(4),
            mode: SpiMode::Mode0,
        },
    };
    LOOPBACK_SPI.get_or_try_init(|| Ok(spi_bus::SpiBus::new(esp32_spi::Esp32Spi::open(&port)?)))
}

/// The board's addressable LED, if it has one.
///
/// This returns the LED's *identity* — its pin, count and fold — not a driven
/// strip. Driving a WS2812 over the RMT needs an interrupt handler and a frame
/// buffer sized to the LED count, which an application owns (see
/// `apps/examples/blink`); the board's job is to say which LED is there. An
/// accessor that returned a live strip would have to move that ISR into the
/// board crate, which no board consumer wants today.
#[cfg(feature = "esp32-drivers")]
pub fn led() -> Option<RgbLed> {
    active::BOARD.rgb_led
}

// ── Manifest invariant tests ────────────────────────────────────────────────
//
// Run against whichever board is currently selected (`crate::active`), so
// `cargo test -p board --no-default-features --features <board>`
// checks that board's manifest. These exist to catch copy-paste errors —
// e.g. a base address copied from the wrong bus, a pin number that isn't a
// real GPIO, two buses accidentally sharing a name, or a device pointing at
// a bus that was renamed/removed — which is exactly how board manifests
// tend to go wrong when a new one is cloned from an existing file.
#[cfg(test)]
mod tests {
    extern crate std;

    use crate::active::*;
    use hal::bus::{BusConfig, I2cConfig, SpiConfig, UartConfig};
    use hal::soc::SystemOnChip as _;

    // The peripheral window and GPIO ceiling are chip facts, so they come from
    // the selected SoC's `SystemOnChip` impl rather than being keyed on the
    // board name here. The board type is the one thing that still varies by
    // family, so it is picked by which soc crate this board pulls in.
    #[cfg(feature = "board-wio-rp2040-mini")]
    use soc_rp2040::Rp2040 as SelectedSoc;
    #[cfg(not(feature = "board-wio-rp2040-mini"))]
    use soc_esp32::Esp32 as SelectedSoc;

    const PERIPH_BASE_LOW: u32 = SelectedSoc::PERIPHERAL_WINDOW.0;
    const PERIPH_BASE_HIGH: u32 = SelectedSoc::PERIPHERAL_WINDOW.1;
    const MAX_GPIO: u8 = SelectedSoc::MAX_GPIO;

    #[test]
    fn board_name_non_empty() {
        assert!(!BOARD_NAME.is_empty());
    }

    #[test]
    fn bus_base_addrs_are_plausible() {
        for bus in TARGET_BUSES {
            assert!(
                bus.base_addr >= PERIPH_BASE_LOW && bus.base_addr <= PERIPH_BASE_HIGH,
                "bus '{}' has base_addr {:#010x} outside the selected SoC peripheral window",
                bus.name,
                bus.base_addr,
            );
        }
    }

    #[test]
    fn peripheral_base_addrs_are_plausible() {
        for p in TARGET_PERIPHERALS {
            assert!(
                p.base_addr >= PERIPH_BASE_LOW && p.base_addr <= PERIPH_BASE_HIGH,
                "peripheral '{}' has base_addr {:#010x} outside the selected SoC peripheral window",
                p.name,
                p.base_addr,
            );
        }
    }

    #[test]
    fn uart_pins_are_valid_gpios() {
        for bus in TARGET_BUSES {
            if let BusConfig::Uart(UartConfig { tx, rx, .. }) = bus.config {
                assert!(tx <= MAX_GPIO, "bus '{}' uart tx pin {} is not a valid GPIO", bus.name, tx);
                assert!(rx <= MAX_GPIO, "bus '{}' uart rx pin {} is not a valid GPIO", bus.name, rx);
            }
        }
    }

    #[test]
    fn spi_and_i2c_pins_are_valid_gpios() {
        for bus in TARGET_BUSES {
            match bus.config {
                BusConfig::Spi(SpiConfig { mosi, miso, sck, .. }) => {
                    for (label, pin) in [("mosi", mosi), ("miso", miso), ("sck", sck)] {
                        assert!(
                            pin <= MAX_GPIO,
                            "bus '{}' spi {} pin {} is not a valid GPIO",
                            bus.name,
                            label,
                            pin
                        );
                    }
                }
                BusConfig::I2c(I2cConfig { sda, scl, .. }) => {
                    for (label, pin) in [("sda", sda), ("scl", scl)] {
                        assert!(
                            pin <= MAX_GPIO,
                            "bus '{}' i2c {} pin {} is not a valid GPIO",
                            bus.name,
                            label,
                            pin
                        );
                    }
                }
                BusConfig::Uart(_) => {}
            }
        }
    }

    #[test]
    fn bus_names_are_unique() {
        for (i, bus) in TARGET_BUSES.iter().enumerate() {
            for other in &TARGET_BUSES[i + 1..] {
                assert_ne!(
                    bus.name, other.name,
                    "duplicate bus name '{}' in TARGET_BUSES",
                    bus.name,
                );
            }
        }
    }

    #[test]
    fn peripheral_names_are_unique() {
        for (i, p) in TARGET_PERIPHERALS.iter().enumerate() {
            for other in &TARGET_PERIPHERALS[i + 1..] {
                assert_ne!(
                    p.name, other.name,
                    "duplicate peripheral name '{}' in TARGET_PERIPHERALS",
                    p.name,
                );
            }
        }
    }

    #[test]
    fn device_entries_reference_an_existing_bus() {
        for device in TARGET_DEVICES {
            assert!(
                TARGET_BUSES.iter().any(|b| b.name == device.bus),
                "device '{}' references bus '{}', which is not in TARGET_BUSES",
                device.name,
                device.bus,
            );
        }
    }

    #[test]
    fn device_cs_pin_is_valid_gpio_when_present() {
        for device in TARGET_DEVICES {
            if let Some(cs) = device.cs_pin {
                assert!(cs <= MAX_GPIO, "device '{}' cs_pin {} is not a valid GPIO", device.name, cs);
            }
        }
    }
}
