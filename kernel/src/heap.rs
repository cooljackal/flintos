// SPDX-License-Identifier: Apache-2.0

//! The radio heap: which memory it gets, and which flavour it hands back.
//!
//! [`heap`] is the allocator; this is the part that knows the ESP32's map.
//! It exists for the radio blobs and nothing else — see the ground rule in
//! `doc/plan-radio.md`. The kernel remains statically allocated, so bounded
//! latency stays a property of the kernel rather than a hope about the blob.
//!
//! # Reclaimed, not placed
//!
//! Wi-Fi wants roughly 50 KB and nothing that size fits below the
//! `0x3FFDC200` bound — the static map leaves a few kilobytes spare. The way
//! out is the memory the linker script already describes as reclaimable: the
//! ROM keeps its data and stack above that bound *during boot* and has no
//! claim on it afterwards.
//!
//! So the regions below are added at runtime, once, after boot. Nothing is
//! placed there at link time and the static map does not move.
//!
//! # All of it is DMA-capable
//!
//! An earlier version of this module split the memory in two, on the belief
//! that the DMA engines could not reach SRAM1. They can, and esp-idf says so
//! plainly:
//!
//! ```c
//! #define SOC_DMA_LOW  0x3FFAE000
//! #define SOC_DMA_HIGH 0x40000000
//!
//! inline static bool IRAM_ATTR esp_ptr_dma_capable(const void *p)
//! {
//!     return (intptr_t)p >= SOC_DMA_LOW && (intptr_t)p < SOC_DMA_HIGH;
//! }
//! ```
//!
//! Its heap agrees — the SRAM1 regions are type 1, whose capability list is
//! `MALLOC_CAP_DMA|MALLOC_CAP_8BIT|MALLOC_CAP_INTERNAL|MALLOC_CAP_DEFAULT` —
//! and NuttX puts ordinary heap regions at `0x3ffe0450` onward. The belief
//! came from a comment in this project's own linker script and was simply
//! wrong. The corrected window is supplied by the selected SoC.
//!
//! So there is one pool and every byte of it can back a DMA descriptor.
//! [`Caps`] survives because the blob's allocator API asks in those terms and
//! the adapter has to answer, not because the two mean different memory here.
//!
//! # What is *not* here
//!
//! No `#[global_allocator]`. Registering one would put `alloc::` in reach of
//! every crate in the tree and quietly end the no-allocation property. Callers
//! name this module.

use portable_atomic::{AtomicBool, Ordering};

use heap::Heap;

use crate::smp::Spinlock;

// The five constants below are read by the target path and by the tests that
// pin them. A host build takes a static buffer instead, so they are dead
// there — kept rather than gated, because they are facts about the chip.
/// End of SRAM2, where SRAM1 begins. Not a DMA boundary — see the module docs.
#[cfg(any(all(feature = "soc-esp32", target_os = "none"), test))]
const SRAM2_END: u32 = 0x3FFE_0000;

/// SRAM1, which the ROM uses during boot and FlintOS never places anything in.
#[cfg(any(all(feature = "soc-esp32", target_os = "none"), test))]
const SRAM1_START: u32 = 0x3FFE_0000;
#[cfg(any(all(feature = "soc-esp32", target_os = "none"), test))]
const SRAM1_END: u32 = 0x4000_0000;

/// The ROM's own data, which stays reserved after boot.
///
/// esp-idf keeps these out of the heap permanently, and they are inside SRAM1
/// rather than at either end — which is why SRAM1 is added as two pieces with
/// a hole between them:
///
/// ```c
/// SOC_RESERVE_MEMORY_REGION(0x3ffe0000, 0x3ffe0440, rom_pro_data);
/// SOC_RESERVE_MEMORY_REGION(0x3ffe3f20, 0x3ffe4350, rom_app_data);
/// ```
///
/// NuttX reserves a little more here than esp-idf does. Where the two differ
/// this follows esp-idf, which is the narrower claim and the one whose
/// boundaries are quoted above; if a ROM routine is ever seen scribbling past
/// them, NuttX's `memory_layout.h` is the place to compare against.
#[cfg(any(all(feature = "soc-esp32", target_os = "none"), test))]
const ROM_PRO_DATA: (u32, u32) = (0x3FFE_0000, 0x3FFE_0440);
#[cfg(any(all(feature = "soc-esp32", target_os = "none"), test))]
const ROM_APP_DATA: (u32, u32) = (0x3FFE_3F20, 0x3FFE_4350);

/// What a caller needs from the memory, beyond its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caps {
    /// Any internal RAM. What most blob allocations want.
    Internal,
    /// Reachable by the DMA engines. On this part that is the same memory —
    /// the distinction is kept because the blob asks in these terms.
    Dma,
}

/// The heap: the SRAM2 tail plus SRAM1, minus the ROM's data.
static POOL: Spinlock<Heap> = Spinlock::new(Heap::new());

/// Set once [`init`] has run, so a second call cannot hand the same memory out
/// twice.
static READY: AtomicBool = AtomicBool::new(false);

/// Take the reclaimable memory and make it allocatable.
///
/// Call once, after boot, before the radio starts. Returns the bytes taken.
///
/// # Safety
/// The ROM must be finished with its boot-time data — that is, this must not
/// run during early startup — and nothing else may be using these regions.
/// `free_from` is the first free address above the static map, which the
/// linker script provides as `_dma_pool_end`.
#[allow(clippy::needless_return)]
pub unsafe fn init(free_from: u32) -> usize {
    if READY.swap(true, Ordering::SeqCst) {
        return 0;
    }

    // A host build has no SRAM1 to reclaim, and adding those addresses would
    // hand out pointers into nothing. It gets a static buffer instead, so the
    // dynamic-object tests exercise the real allocator rather than a stand-in.
    #[cfg(not(target_os = "none"))]
    {
        let _ = free_from;
        const TEST_HEAP: usize = 256 * 1024;
        static mut BUF: [u8; TEST_HEAP] = [0; TEST_HEAP];
        return POOL.with(|pool| unsafe { pool.add_region((&raw mut BUF) as *mut u8, TEST_HEAP) });
    }

    #[cfg(all(target_os = "none", feature = "soc-esp32"))]
    return POOL.with(|pool| {
        let mut taken = 0;

        // Whatever SRAM2 has left above the static map.
        if free_from < SRAM2_END {
            taken +=
                unsafe { pool.add_region(free_from as *mut u8, (SRAM2_END - free_from) as usize) };
        }

        // SRAM1 in two pieces, because the ROM's data sits in the middle of
        // it. Adding it as one span would hand out allocations straddling
        // memory that is not ours — and the allocator must not coalesce across
        // the gap either, which is why `add_region` is called twice rather
        // than once with a hole punched out of it.
        for (start, end) in [
            (ROM_PRO_DATA.1, ROM_APP_DATA.0),
            (ROM_APP_DATA.1, SRAM1_END),
        ] {
            debug_assert!(start >= SRAM1_START && end <= SRAM1_END && start < end);
            taken += unsafe { pool.add_region(start as *mut u8, (end - start) as usize) };
        }
        taken
    });

    #[cfg(all(target_os = "none", feature = "soc-rp2040"))]
    return POOL.with(|pool| match rp2040_heap_region(free_from) {
        Some((start, len)) => unsafe { pool.add_region(start as *mut u8, len) },
        None => 0,
    });
}

#[cfg(all(feature = "soc-rp2040", any(target_os = "none", test)))]
fn rp2040_heap_region(free_from: u32) -> Option<(u32, usize)> {
    (free_from < soc_rp2040::SRAM_END)
        .then_some((free_from, (soc_rp2040::SRAM_END - free_from) as usize))
}

/// [`init`], with `free_from` taken from the linker script.
///
/// `_dma_pool_end` is the first address above the static map. Asking the
/// linker rather than repeating a constant means the map can move — and it
/// does, under `radio-bt` — without every caller going stale. Two callers
/// already: the self-test suite and any application that brings the radio up.
///
/// # Safety
/// As [`init`].
#[cfg(target_os = "none")]
pub unsafe fn init_from_map() -> usize {
    unsafe extern "C" {
        static _dma_pool_end: u8;
    }
    unsafe { init((&raw const _dma_pool_end) as u32) }
}

/// Allocate from the pool.
///
/// # Safety
/// `align` must be a power of two. The returned pointer must be released with
/// [`free`].
pub unsafe fn alloc(size: usize, align: usize) -> *mut u8 {
    POOL.with(|h| unsafe { h.alloc(size, align) })
}

/// Allocate memory a DMA engine can reach.
///
/// # Safety
/// As [`alloc`].
pub unsafe fn alloc_dma(size: usize, align: usize) -> *mut u8 {
    let p = POOL.with(|h| unsafe { h.alloc(size, align) });
    // Every region the pool holds is inside the DMA window, so this cannot
    // fire unless `init` was given a bad bound. It is cheap, and the failure
    // it guards against is silent corruption rather than a fault.
    debug_assert!(p.is_null() || is_dma_capable(p));
    p
}

/// Allocate for the given capability.
///
/// # Safety
/// As [`alloc`].
pub unsafe fn alloc_caps(size: usize, align: usize, caps: Caps) -> *mut u8 {
    match caps {
        Caps::Internal => unsafe { alloc(size, align) },
        Caps::Dma => unsafe { alloc_dma(size, align) },
    }
}

/// Grow or shrink an allocation, preserving its contents.
///
/// The radio blobs' `_realloc_internal` and `_wifi_realloc`. See
/// [`heap::Heap::realloc`] for the two C edges it honours — a failed call
/// leaves the original valid, and a size of zero frees.
///
/// # Safety
/// `ptr` is null or from this module and not yet freed. `align` must be a
/// power of two.
pub unsafe fn realloc(ptr: *mut u8, size: usize, align: usize) -> *mut u8 {
    POOL.with(|h| unsafe { h.realloc(ptr, size, align) })
}

/// Return an allocation to the pool.
///
/// # Safety
/// `ptr` must have come from this module and not been freed already.
pub unsafe fn free(ptr: *mut u8, caps: Caps) {
    // One pool, so the capability does not choose an allocator. It is still
    // taken, so callers stay in the habit of pairing it with the allocation —
    // which matters if this ever does split again.
    let _ = caps;
    POOL.with(|h| unsafe { h.free(ptr) })
}

/// Whether an address is one a DMA engine can reach.
///
/// Delegates to the SoC crate, so there is one definition of the window rather
/// than a copy here that can drift away from it. This used to say exactly that
/// while the host arm of a `cfg` held a second copy of the range -- the drift
/// the comment warned about, already present. `soc-esp32` is an unconditional
/// dependency now, so both builds ask the same function.
pub fn is_dma_capable(ptr: *const u8) -> bool {
    use hal::dma::DmaReach;
    use hal::soc::SystemOnChip;

    crate::board::SelectedSoc::DMA.reachable(ptr as u32, 1)
}

/// Free bytes in the pool.
///
/// The sum of the free blocks, which is what `get_free_heap_size` means. It is
/// not a promise that a request of this size will succeed — see
/// [`largest_free_block`].
pub fn free_bytes(caps: Caps) -> usize {
    let _ = caps;
    POOL.with(|h| h.free_bytes())
}

/// Total bytes under management.
pub fn total_bytes(caps: Caps) -> usize {
    let _ = caps;
    POOL.with(|h| h.total_bytes())
}

/// The largest allocation that can currently succeed, ignoring the header.
pub fn largest_free_block(caps: Caps) -> usize {
    let _ = caps;
    POOL.with(|h| h.largest_free_block())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes SRAM1 yields once the ROM's two holes are removed.
    fn sram1_usable() -> u32 {
        (ROM_APP_DATA.0 - ROM_PRO_DATA.1) + (SRAM1_END - ROM_APP_DATA.1)
    }

    #[test]
    fn the_rom_holes_are_inside_sram1_and_ordered() {
        // If these ever overlap or invert, `init` would add a region with a
        // negative length, or hand out the ROM's data.
        assert!(ROM_PRO_DATA.0 >= SRAM1_START);
        assert!(ROM_PRO_DATA.0 < ROM_PRO_DATA.1);
        assert!(ROM_PRO_DATA.1 <= ROM_APP_DATA.0);
        assert!(ROM_APP_DATA.0 < ROM_APP_DATA.1);
        assert!(ROM_APP_DATA.1 < SRAM1_END);
    }

    #[test]
    fn sram1_is_big_enough_for_the_radio() {
        // Wi-Fi wants roughly 50 KB. If a future reservation eats into this,
        // the plan needs revisiting rather than discovering it at runtime.
        let usable = sram1_usable();
        assert!(
            usable >= 100 * 1024,
            "SRAM1 yields only {usable} bytes after the ROM's reservations"
        );
    }

    #[test]
    #[cfg(feature = "soc-esp32")]
    fn every_byte_the_pool_can_hold_is_dma_capable() {
        // The correction that collapsed two pools into one. SRAM1 was believed
        // unreachable by DMA; esp-idf's SOC_DMA_HIGH is 0x40000000, so the
        // whole pool qualifies. If this ever fails, `alloc_dma` has to go back
        // to a region of its own.
        for addr in [
            0x3FFD_C200u32, // the SRAM2 tail, just above the static bound
            SRAM2_END - 4,
            SRAM1_START,
            ROM_PRO_DATA.1,
            ROM_APP_DATA.1,
            SRAM1_END - 4,
        ] {
            assert!(
                is_dma_capable(addr as *const u8),
                "{addr:#x} should be DMA-capable"
            );
        }
    }

    #[test]
    #[cfg(feature = "soc-rp2040")]
    fn rp2040_dma_reach_uses_the_sram_window() {
        assert!(is_dma_capable(0x2000_0000 as *const u8));
        assert!(is_dma_capable(0x2004_1FFF as *const u8));
        assert!(!is_dma_capable(0x1FFF_FFFF as *const u8));
        assert!(!is_dma_capable(0x2004_2000 as *const u8));
    }

    #[test]
    fn addresses_outside_internal_dram_are_not_dma_capable() {
        for addr in [0x3FFA_DFFCu32, 0x4000_0000, 0x4008_0000, 0x3F40_0000] {
            assert!(
                !is_dma_capable(addr as *const u8),
                "{addr:#x} should not be DMA-capable"
            );
        }
    }

    #[test]
    fn the_two_regions_meet_without_a_gap_or_an_overlap() {
        // SRAM2 ends exactly where SRAM1 begins; a gap or an overlap here
        // would either lose memory or double-issue it.
        assert_eq!(SRAM2_END, SRAM1_START);
    }

    #[test]
    fn a_bound_past_sram2_yields_nothing_rather_than_wrapping() {
        // `init` computes `SRAM2_END - free_from`. If the static map ever grew
        // past SRAM2 that subtraction would underflow, so the guard matters.
        let free_from = SRAM2_END + 0x1000;
        assert!(free_from >= SRAM2_END, "the guard's condition");
    }

    #[cfg(feature = "soc-rp2040")]
    #[test]
    fn rp2040_heap_uses_only_sram_after_the_static_map() {
        assert_eq!(rp2040_heap_region(0x2004_0000), Some((0x2004_0000, 0x2000)));
        assert_eq!(rp2040_heap_region(soc_rp2040::SRAM_END), None);
    }
}
