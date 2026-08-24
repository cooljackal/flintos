// SPDX-License-Identifier: Apache-2.0

//! Where the internal DRAM regions go, as a function of which radios are on.
//!
//! This exists because of one fact about the ESP32's Bluetooth controller: it
//! wants the bottom of DRAM, and it wants it at link time. esp-idf carves it
//! off the origin of the static segment —
//!
//! ```text
//! dram0_0_seg (RW) : org = 0x3FFB0000 + CONFIG_BTDM_RESERVE_DRAM,
//!                    len = DRAM0_0_SEG_LEN - CONFIG_BTDM_RESERVE_DRAM
//! ```
//!
//! — and sizes it from Kconfig:
//!
//! ```text
//! config BTDM_RESERVE_DRAM
//!     hex
//!     default 0xdb5c if BT_ENABLED
//!     default 0
//! ```
//!
//! Two things follow, and both are the reason this is a computed map rather
//! than a hand-edited one.
//!
//! **The cost is binary, not additive.** `0xdb5c` is the same for BLE-only,
//! BR/EDR-only and dual-mode, and Wi-Fi adds nothing at all. So there is one
//! shifted map, not one per radio combination, and enabling Wi-Fi alongside
//! Bluetooth costs no extra DRAM.
//!
//! **Wi-Fi-only must not pay for it.** A build without Bluetooth produces
//! byte-for-byte the map FlintOS has always had — see
//! `the_default_map_is_unchanged`, which pins the literal addresses so that a
//! refactor here cannot quietly move a working board's memory.
//!
//! # The squeeze
//!
//! Everything static must sit below `0x3FFDC200`; above that is the ROM's own
//! data and stack during boot. That leaves 176.5 KiB, of which Bluetooth takes
//! 56 KiB, so the remaining regions have to give up 52 KiB between them. The
//! per-task stack pool takes most of the cut, 96 KiB down to 80 KiB, and
//! `dram_seg` drops from 64 KiB to 28 KiB — comfortable, since it holds about
//! 22 KiB of `.data`, `.bss` and the boot stack.
//!
//! A build that overflows fails at link time, not at boot: see the `ASSERT`s
//! emitted alongside the `MEMORY` block.

use core::fmt::Write as _;

/// Bottom of internal DRAM (SRAM2).
pub const DRAM_BASE: u32 = 0x3FFB_0000;

/// Nothing statically placed may cross this. The ROM keeps its data and stack
/// above it during boot, which is why Espressif's own template caps static
/// DRAM at exactly `0x3FFB0000 + 0x2C200`.
pub const SAFE_END: u32 = 0x3FFD_C200;

/// End of SRAM2. Not a DMA boundary: the engines reach all of internal DRAM,
/// up to `0x40000000` (esp-idf's `SOC_DMA_HIGH`). This matters here only
/// because every statically placed region has to sit below the ROM's
/// `0x3FFDC200` bound anyway, which is tighter than either.
pub const SRAM2_END: u32 = 0x3FFE_0000;

/// `CONFIG_BTDM_RESERVE_DRAM`, the controller's own static DRAM.
pub const BTDM_RESERVE_DRAM: u32 = 0xDB5C;

/// DMA pool size, radio-keyed. The pool is the last region, so it grows into
/// whatever slack is left under [`SAFE_END`] without moving anything below it.
///
/// A Wi-Fi-only build has ~4.5 KiB of that slack, so the pool takes 12 KiB —
/// enough for the display to DMA in 4 KiB chunks (#140) rather than the 2 KiB
/// the old fixed 8 KiB forced. A Bluetooth build has ~0.5 KiB of slack once the
/// controller's reservation and the trimmed stacks are placed, so it keeps
/// 8 KiB; growing it there would have to come out of `task_stacks`. `problem()`
/// validates both against the bound regardless — this is a tuned size, not a
/// trusted one.
pub const DMA_POOL_WIFI_ONLY: u32 = 0x3000; // 12 KiB
pub const DMA_POOL_WITH_BT: u32 = 0x2000; // 8 KiB

/// What we actually reserve: `BTDM_RESERVE_DRAM` rounded up to a clean
/// boundary, so every following origin stays readable in a hex dump.
pub const BT_RESERVE: u32 = 0xE000;

/// One contiguous region of the map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub name: &'static str,
    pub origin: u32,
    pub length: u32,
    pub note: &'static str,
}

impl Region {
    pub fn end(&self) -> u32 {
        self.origin + self.length
    }
}

/// The DRAM layout for one build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DramMap {
    /// Bytes handed to the Bluetooth controller at the bottom of DRAM, or zero.
    pub bt_reserve: u32,
    pub regions: [Region; 4],
}

impl DramMap {
    /// Build the map for a given radio configuration.
    ///
    /// `bluetooth` is "the BT controller is enabled in any mode" — not "BLE".
    /// Naming the reservation after one mode would mean rewriting this the day
    /// BR/EDR appears, and the reservation is identical for both.
    pub fn new(bluetooth: bool) -> Self {
        let bt_reserve = if bluetooth { BT_RESERVE } else { 0 };

        // `dram_seg` holds .data, .bss and the 8 KiB boot stack — about 22 KiB
        // today. `task_stacks` is the only region big enough to absorb the
        // reservation, so it takes the cut.
        let (dram_len, stacks_len) = if bluetooth {
            (0x7000, 0x14000) // 28 KiB, 80 KiB
        } else {
            (0x10000, 0x18000) // 64 KiB, 96 KiB
        };
        let dma_len = if bluetooth { DMA_POOL_WITH_BT } else { DMA_POOL_WIFI_ONLY };

        let mut at = DRAM_BASE + bt_reserve;
        let mut place = |name, length, note| {
            let r = Region { name, origin: at, length, note };
            at += length;
            r
        };

        Self {
            bt_reserve,
            regions: [
                place("dram_seg", dram_len, ".data + .bss + kernel/boot stack"),
                place("task_stacks", stacks_len, "per-task stacks"),
                place("panic_region", 0x1000, "survives soft reset"),
                place("dma_pool", dma_len, "DMA-safe buffers"),
            ],
        }
    }

    fn get(&self, name: &str) -> &Region {
        self.regions
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("no region named {name}"))
    }

    /// First address past the last region.
    pub fn end(&self) -> u32 {
        self.regions.last().expect("regions is non-empty").end()
    }

    /// Why this map is unusable, if it is. Checked here so a bad edit fails
    /// `cargo test` on a laptop rather than at link time on someone's board.
    pub fn problem(&self) -> Option<String> {
        if self.bt_reserve != 0 && self.bt_reserve < BTDM_RESERVE_DRAM {
            return Some(format!(
                "BT reservation {:#x} is below CONFIG_BTDM_RESERVE_DRAM {:#x}",
                self.bt_reserve, BTDM_RESERVE_DRAM
            ));
        }
        if self.end() > SAFE_END {
            return Some(format!(
                "static DRAM ends at {:#x}, past the {:#x} bound by {} bytes",
                self.end(),
                SAFE_END,
                self.end() - SAFE_END
            ));
        }
        // Not a DMA bound -- the engines reach to 0x40000000. This is here
        // because a `dma_pool` that had climbed past SRAM2 would mean the
        // static map had escaped the ROM bound above, and a second, blunter
        // check costs nothing.
        let dma = self.get("dma_pool");
        if dma.end() > SRAM2_END {
            return Some(format!(
                "dma_pool ends at {:#x}, past the end of SRAM2",
                dma.end()
            ));
        }
        None
    }

    /// The `MEMORY` entries and the bound checks, as linker-script text.
    pub fn to_linker_text(&self) -> String {
        let mut s = String::new();
        if self.bt_reserve != 0 {
            let _ = writeln!(
                s,
                "  /* {:#010X}  (bt_reserve)  {:>3}K   Bluetooth controller, \
                 CONFIG_BTDM_RESERVE_DRAM = {:#x} */",
                DRAM_BASE,
                self.bt_reserve / 1024,
                BTDM_RESERVE_DRAM
            );
        }
        for r in &self.regions {
            let _ = writeln!(
                s,
                "  {:<12}(RW) : ORIGIN = {:#010X}, LENGTH = {:#07X}    /* {:>3}K {} */",
                r.name,
                r.origin,
                r.length,
                r.length / 1024,
                r.note
            );
        }
        s
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_map_keeps_every_origin() {
        // The literal ORIGINs FlintOS has always used. The promise this test
        // guards is that no region below `dma_pool` ever moves for a build
        // without Bluetooth. `dma_pool` is last, so #140 growing it 8K -> 12K
        // extends only its own end (checked against `SAFE_END` by `problem`)
        // and shifts nothing beneath it.
        let m = DramMap::new(false);
        assert_eq!(m.bt_reserve, 0);
        let want = [
            ("dram_seg", 0x3FFB_0000u32, 0x10000u32),
            ("task_stacks", 0x3FFC_0000, 0x18000),
            ("panic_region", 0x3FFD_8000, 0x1000),
            ("dma_pool", 0x3FFD_9000, DMA_POOL_WIFI_ONLY),
        ];
        for (r, (name, origin, length)) in m.regions.iter().zip(want) {
            assert_eq!(r.name, name);
            assert_eq!(r.origin, origin, "{name} origin");
            assert_eq!(r.length, length, "{name} length");
        }
        assert_eq!(m.problem(), None);
    }

    #[test]
    fn the_bluetooth_map_fits_under_the_bound() {
        let m = DramMap::new(true);
        assert_eq!(m.problem(), None, "{:?}", m.problem());
        assert!(
            m.end() <= SAFE_END,
            "ends at {:#x}, bound is {SAFE_END:#x}",
            m.end()
        );
        // Bluetooth starts where the reservation ends, not at DRAM_BASE.
        assert_eq!(m.regions[0].origin, DRAM_BASE + BT_RESERVE);
    }

    #[test]
    fn the_reservation_covers_what_esp_idf_asks_for() {
        // Reserving less than CONFIG_BTDM_RESERVE_DRAM gives memory that works
        // until the controller initialises and then corrupts static data --
        // the failure this whole computation exists to prevent.
        // `assert!` on two consts is a lint error, and rightly: it is checked
        // at compile time and proves nothing at run time. A const assertion
        // says the same thing where it belongs, and fails the *build*.
        const _: () = assert!(BT_RESERVE >= BTDM_RESERVE_DRAM);
        assert_eq!(DramMap::new(true).bt_reserve, BT_RESERVE);
        assert_eq!(DramMap::new(false).bt_reserve, 0);
    }

    #[test]
    fn regions_are_contiguous_and_ascending_in_both_maps() {
        for bluetooth in [false, true] {
            let m = DramMap::new(bluetooth);
            let mut at = DRAM_BASE + m.bt_reserve;
            for r in &m.regions {
                assert_eq!(r.origin, at, "{} is not contiguous (bt={bluetooth})", r.name);
                assert!(r.length > 0, "{} is empty", r.name);
                at = r.end();
            }
        }
    }

    #[test]
    fn the_dma_pool_stays_inside_sram2() {
        // Outside SRAM2 a DMA buffer fails silently rather than erroring,
        // which is the documented failure mode for `dma_broker`.
        for bluetooth in [false, true] {
            let m = DramMap::new(bluetooth);
            let dma = m.get("dma_pool");
            assert!(dma.origin >= DRAM_BASE);
            assert!(dma.end() <= SRAM2_END, "bt={bluetooth}");
            assert!(dma.length >= 4096);
        }
    }

    #[test]
    fn an_overflowing_map_is_reported_rather_than_emitted() {
        // Hand-built, because the real maps both fit: the point is that
        // `problem` actually notices.
        let mut m = DramMap::new(true);
        m.regions[1].length += 0x8000;
        m.regions[2].origin += 0x8000;
        m.regions[3].origin += 0x8000;
        let problem = m.problem().expect("overflow should be reported");
        assert!(problem.contains("past the"), "{problem}");
    }

    #[test]
    fn the_generated_text_names_every_region() {
        for bluetooth in [false, true] {
            let text = DramMap::new(bluetooth).to_linker_text();
            for name in ["dram_seg", "task_stacks", "panic_region", "dma_pool"] {
                assert!(text.contains(name), "{name} missing (bt={bluetooth})");
            }
            assert_eq!(text.contains("bt_reserve"), bluetooth);
        }
    }
}
