// SPDX-License-Identifier: Apache-2.0

//! Which radios this build uses, and whether the board has them.
//!
//! # The feature set
//!
//! | Feature | Means |
//! |---|---|
//! | `radio-wifi` | Wi-Fi. Costs no static DRAM |
//! | `radio-bt` | The Bluetooth controller is on, in some mode. Reserves DRAM |
//! | `radio-ble` | BLE. Implies `radio-bt` |
//! | `radio-bt-classic` | BR/EDR. Implies `radio-bt` |
//!
//! `radio-bt` is not meant to be selected directly — `radio-ble` and
//! `radio-bt-classic` turn it on. It exists as a separate feature because the
//! thing that costs memory is *the controller being enabled*, not which mode
//! it runs in:
//!
//! ```text
//! config BTDM_RESERVE_DRAM
//!     hex
//!     default 0xdb5c if BT_ENABLED
//!     default 0
//! ```
//!
//! Identical for BLE-only, BR/EDR-only and dual-mode. Keying the memory map on
//! `radio-ble` would have meant rewriting the linker script the day BR/EDR
//! appeared, for no change in the number.
//!
//! Enabling both modes is legal and is how dual-mode is selected. It reserves
//! no more DRAM than either alone, but it does forfeit the claw-back described
//! below.
//!
//! # What is not here
//!
//! **BLE Mesh** is not a build flag at this layer. It is a host stack above
//! BLE, not a controller mode, so it costs heap rather than static DRAM and
//! would arrive as a crate on top of `radio-ble`. `doc/plan-radio.md` lists it
//! as a non-goal for now regardless.
//!
//! # The claw-back
//!
//! A BLE-only build can hand the unused BR/EDR sections back at runtime —
//! `esp_bt_controller_mem_release(ESP_BT_MODE_CLASSIC_BT)`, which esp-idf
//! documents as releasing "the BSS, data and other sections of the controller
//! to heap. The total size is about 70k bytes." It is irreversible, which is
//! fine when the mode is fixed at build time. [`RELEASE_CLASSIC_MEMORY`] says
//! whether this build may do it.

/// Wi-Fi is in use.
pub const WIFI: bool = cfg!(feature = "radio-wifi");

/// The Bluetooth controller is enabled in some mode. This is the flag the
/// memory map is keyed on — see `tools/build/src/map.rs`.
pub const BT: bool = cfg!(feature = "radio-bt");

/// BLE specifically.
pub const BLE: bool = cfg!(feature = "radio-ble");

/// BR/EDR ("Bluetooth Classic") specifically.
pub const BT_CLASSIC: bool = cfg!(feature = "radio-bt-classic");

/// Whether the BR/EDR sections may be released to the heap at init.
///
/// Only when Bluetooth is on and Classic is not wanted. Releasing while
/// Classic is in use would free memory the controller is about to rely on, and
/// the operation cannot be undone.
pub const RELEASE_CLASSIC_MEMORY: bool = BT && !BT_CLASSIC;

// ── The board must actually have the radio ──────────────────────────────────
//
// `compile_error!` cannot see a board constant, so these are const assertions:
// a false one fails const evaluation, which is a build error naming the
// problem. Same intent as the "exactly one board" guards in `board`.

#[cfg(feature = "radio-wifi")]
const _: () = assert!(
    board::active::HAS_WIFI,
    "radio-wifi is enabled but the selected board declares no Wi-Fi radio \
     (board::active::HAS_WIFI is false)"
);

#[cfg(feature = "radio-bt")]
const _: () = assert!(
    board::active::HAS_BT,
    "a Bluetooth feature is enabled but the selected board declares no \
     Bluetooth radio (board::active::HAS_BT is false)"
);

// `radio-bt` is the memory-map flag and must never be on without a mode: the
// build would reserve 56 KiB of DRAM and shrink the task stack pool to pay for
// a controller nothing then enables.
#[cfg(all(
    feature = "radio-bt",
    not(any(feature = "radio-ble", feature = "radio-bt-classic"))
))]
compile_error!(
    "kernel: radio-bt is enabled without a mode. It is turned on by
     radio-ble or radio-bt-classic and is not meant to be selected on its
     own -- as it stands this build reserves DRAM for a controller it never
     enables. Use:

     	make ... EXTRA_FEATURES=radio-ble
     	make ... EXTRA_FEATURES=radio-wifi,radio-ble"
);

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
// Every value here is a `cfg!`, so for any single build these assertions are
// constant — which is the point. They encode invariants that must hold for
// *each* feature combination, and the suite is run across combinations rather
// than relying on one build to exercise them all.
#[allow(clippy::assertions_on_constants)]
mod tests {
    use super::*;

    #[test]
    fn a_mode_implies_the_controller() {
        // Cargo's feature unification is what actually enforces this; the test
        // is here so a hand-edited Cargo.toml that drops the implication is
        // caught rather than silently producing an unreserved map.
        if BLE || BT_CLASSIC {
            assert!(BT, "a Bluetooth mode is on but radio-bt is not");
        }
    }

    #[test]
    fn classic_memory_is_only_released_when_classic_is_unused() {
        if RELEASE_CLASSIC_MEMORY {
            assert!(BT);
            assert!(!BT_CLASSIC, "releasing BR/EDR memory while BR/EDR is in use");
        }
    }

    #[test]
    fn wifi_alone_costs_no_bluetooth_reservation() {
        // The property the whole feature split exists for: a Wi-Fi-only build
        // must not move the memory map.
        if WIFI && !BLE && !BT_CLASSIC {
            assert!(!BT, "Wi-Fi alone should not enable the BT controller");
        }
    }
}
