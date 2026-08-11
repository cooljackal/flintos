// SPDX-License-Identifier: Apache-2.0

//! The PHY's initialisation parameters.
//!
//! `esp_phy_init_data_t` — `uint8_t params[128]` — the first argument to
//! `register_chipv7_phy`. Espressif's recommended defaults, transcribed from
//! esp-idf `components/esp_phy/esp32/include/phy_init_data.h` at tag **v4.4**,
//! the revision `tools/fetch-blobs.sh` pins the archives to.
//!
//! # Checked in rather than fetched
//!
//! The archives are fetched because they are eight megabytes of binaries.
//! This is 128 bytes of documented constants from an Apache-2.0 header, and
//! fetching it would need a parser and a generated file in the build — more
//! machinery than it saves. The provenance is recorded instead, and the
//! regeneration recipe is in the tests.
//!
//! # 107 values, 128 bytes
//!
//! The C initialiser lists 107 entries and lets the compiler zero the rest.
//! That is easy to miss and invisible from the header, so the tail is an
//! explicit zero fill here and a test pins it: a table 21 bytes short would be
//! handed to the PHY along with whatever followed it in memory.
//!
//! # Transmit power is a knob, not a constant
//!
//! Six entries are `LIMIT(CONFIG_ESP_PHY_MAX_TX_POWER * 4, 40, n)` in the
//! header — a Kconfig value FlintOS has no equivalent of. It is a regulatory
//! and thermal choice rather than a chip fact, so it is a parameter here and
//! the clamping happens exactly as the macro does it.
//!
//! Units are quarter-dBm, which is the multiply by four. The six cover
//! different modulations and their ceilings differ (78, 72, 66, 60, 56, 52)
//! because the higher-rate ones cannot be driven as hard.

/// The header's `LIMIT(val, low, high)` — a clamp, not a one-sided
/// saturation. Dropping the low bound would under-drive the PHY rather than
/// simply ignoring the setting.
const fn limit(val: i32, low: i32, high: i32) -> u8 {
    if val < low {
        low as u8
    } else if val > high {
        high as u8
    } else {
        val as u8
    }
}

/// `sizeof(esp_phy_init_data_t)`.
pub const PHY_INIT_DATA_LEN: usize = 128;

/// Where the six transmit-power entries sit, from the header's ordering.
///
/// Named so the tests can check the knob reaches them, rather than inferring
/// the positions by differencing two tables and hoping.
pub const TX_POWER_SLOTS: [usize; 6] = [44, 45, 46, 47, 48, 49];

/// The per-modulation ceilings, in quarter-dBm, in slot order.
pub const TX_POWER_CEILINGS: [u8; 6] = [78, 72, 66, 60, 56, 52];

/// The floor every entry is clamped up to.
pub const TX_POWER_FLOOR: u8 = 40;

/// Build the init data for a maximum transmit power, in dBm.
///
/// `const fn`, so a board's table is computed at build time and costs nothing
/// at boot.
pub const fn init_data(max_tx_power_dbm: i32) -> [u8; PHY_INIT_DATA_LEN] {
    let explicit: [u8; 107] = [
    3,
    3,
    5,
    9,
    6,
    5,
    3,
    6,
    5,
    4,
    6,
    4,
    5,
    0,
    0,
    0,
    0,
    5,
    9,
    6,
    5,
    3,
    6,
    5,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    252,
    252,
    254,
    240,
    240,
    240,
    224,
    224,
    224,
    24,
    24,
    24,
    limit(max_tx_power_dbm * 4, 40, 78),   // TX power 0
    limit(max_tx_power_dbm * 4, 40, 72),   // TX power 1
    limit(max_tx_power_dbm * 4, 40, 66),   // TX power 2
    limit(max_tx_power_dbm * 4, 40, 60),   // TX power 3
    limit(max_tx_power_dbm * 4, 40, 56),   // TX power 4
    limit(max_tx_power_dbm * 4, 40, 52),   // TX power 5
    0,
    1,
    1,
    2,
    2,
    3,
    4,
    5,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,    ];
    // C zero-fills what the initialiser does not list. Written out rather
    // than assumed: the difference is 21 bytes of whatever followed it.
    let mut out = [0u8; PHY_INIT_DATA_LEN];
    let mut i = 0;
    while i < 107 {
        out[i] = explicit[i];
        i += 1;
    }
    out
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_the_length_the_phy_expects() {
        // 107 explicit values plus 21 implicit zeros. A short table would be
        // handed to the PHY along with whatever followed it in memory.
        let d = init_data(20);
        assert_eq!(d.len(), PHY_INIT_DATA_LEN);
        assert_eq!(&d[107..], &[0u8; 21], "the tail is C's implicit zero fill");
    }

    #[test]
    fn the_leading_values_match_the_header() {
        // Spot-checked against phy_init_data.h at v4.4. To regenerate:
        //   gh api "repos/espressif/esp-idf/contents/components/esp_phy/\
        //           esp32/include/phy_init_data.h?ref=v4.4" --jq .content \
        //     | base64 -d
        let d = init_data(20);
        assert_eq!(&d[..8], &[3, 3, 5, 9, 6, 5, 3, 6]);
    }

    #[test]
    fn transmit_power_is_clamped_exactly_as_the_macro_does() {
        // IDF's default is 20 dBm: 20 * 4 = 80, above every ceiling, so all
        // six sit at their maximum.
        let d = init_data(20);
        for (n, &slot) in TX_POWER_SLOTS.iter().enumerate() {
            assert_eq!(d[slot], TX_POWER_CEILINGS[n], "slot {slot} at 20 dBm");
        }

        // A low setting is clamped *up* to the floor rather than passed
        // through -- the macro has a low bound, and losing it would
        // under-drive the PHY instead of merely ignoring the request.
        let low = init_data(1);
        for &slot in &TX_POWER_SLOTS {
            assert_eq!(low[slot], TX_POWER_FLOOR, "slot {slot} at 1 dBm");
        }

        // And a value inside every band lands where it was asked to.
        let mid = init_data(12); // 48 quarter-dBm
        for &slot in &TX_POWER_SLOTS {
            assert_eq!(mid[slot], 48, "slot {slot} at 12 dBm");
        }
    }

    #[test]
    fn the_knob_only_moves_the_six_slots() {
        // If it moved anything else, the board's power setting would be
        // changing PHY parameters that have nothing to do with power.
        let a = init_data(20);
        let b = init_data(10);
        for i in 0..PHY_INIT_DATA_LEN {
            if TX_POWER_SLOTS.contains(&i) {
                assert_ne!(a[i], b[i], "slot {i} should track the knob");
            } else {
                assert_eq!(a[i], b[i], "byte {i} must not depend on TX power");
            }
        }
    }

    #[test]
    fn the_ceilings_are_ordered_as_the_header_has_them() {
        // Higher-rate modulations cannot be driven as hard, so the ceilings
        // descend. A transposition here would over-drive one of them.
        for pair in TX_POWER_CEILINGS.windows(2) {
            assert!(pair[0] > pair[1], "ceilings must descend: {pair:?}");
        }
        assert!(TX_POWER_CEILINGS[5] > TX_POWER_FLOOR);
    }
}
