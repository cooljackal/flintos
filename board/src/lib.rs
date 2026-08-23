// SPDX-License-Identifier: Apache-2.0

//! Board manifest for FlintOS.
//!
//! Each supported board is a submodule that exports:
//! - `TARGET_BUSES` — physical bus definitions
//! - `TARGET_DEVICES` — logical device attachments
//! - `TARGET_PERIPHERALS` — direct peripheral mappings
//! - `TARGET_SERVICES` — system service tasks
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

    // These are family facts, not generic manifest invariants. ESP32
    // peripheral registers live in the DPORT-mapped bus window
    // 0x3FF4_0000..0x3FF8_0000. Widened slightly on both sides so this
    // doesn't need updating for every new base address, while still
    // catching a base address copy-pasted from an unrelated address space.
    #[cfg(not(feature = "board-wio-rp2040-mini"))]
    const PERIPH_BASE_LOW: u32 = 0x3FF0_0000;
    #[cfg(not(feature = "board-wio-rp2040-mini"))]
    const PERIPH_BASE_HIGH: u32 = 0x3FF8_FFFF;

    #[cfg(feature = "board-wio-rp2040-mini")]
    const PERIPH_BASE_LOW: u32 = 0x4000_0000;
    #[cfg(feature = "board-wio-rp2040-mini")]
    const PERIPH_BASE_HIGH: u32 = 0x4007_FFFF;

    // ESP32 (and ESP32-PICO-D4) expose GPIO0..=39.
    #[cfg(not(feature = "board-wio-rp2040-mini"))]
    const MAX_GPIO: u8 = 39;
    #[cfg(feature = "board-wio-rp2040-mini")]
    const MAX_GPIO: u8 = 29;

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
    fn service_names_are_unique() {
        for (i, svc) in TARGET_SERVICES.iter().enumerate() {
            for other in &TARGET_SERVICES[i + 1..] {
                assert_ne!(
                    svc.name, other.name,
                    "duplicate service name '{}' in TARGET_SERVICES",
                    svc.name,
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
