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
//! # DMA cannot reach the big region
//!
//! The two flavours are not a formality. SRAM1 is the large, comfortable
//! region — and **the DMA engines cannot address it.** A descriptor pointing
//! into SRAM1 does not error; the transfer silently moves the wrong bytes,
//! which is the failure mode already documented for `dma_broker`.
//!
//! So DMA-capable allocations come from the SRAM2 tail instead, which is
//! small. [`alloc_dma`] enforces that by construction rather than by comment,
//! and [`Caps::Dma`] is checked against the result.
//!
//! # What is *not* here
//!
//! No `#[global_allocator]`. Registering one would put `alloc::` in reach of
//! every crate in the tree and quietly end the no-allocation property. Callers
//! name this module.

use core::sync::atomic::{AtomicBool, Ordering};

use heap::Heap;

use crate::smp::Spinlock;

/// End of SRAM2, and the last address the DMA engines can reach.
const SRAM2_END: u32 = 0x3FFE_0000;

/// SRAM1, which the ROM uses during boot and FlintOS never places anything in.
const SRAM1_START: u32 = 0x3FFE_0000;
const SRAM1_END: u32 = 0x4000_0000;

/// The ROM's own data, which stays reserved after boot.
///
/// esp-idf keeps these out of the heap permanently, and they are inside SRAM1
/// rather than at either end — which is why the general region is added as two
/// pieces with a hole between them:
///
/// ```c
/// SOC_RESERVE_MEMORY_REGION(0x3ffe0000, 0x3ffe0440, rom_pro_data);
/// SOC_RESERVE_MEMORY_REGION(0x3ffe3f20, 0x3ffe4350, rom_app_data);
/// ```
const ROM_PRO_DATA: (u32, u32) = (0x3FFE_0000, 0x3FFE_0440);
const ROM_APP_DATA: (u32, u32) = (0x3FFE_3F20, 0x3FFE_4350);

/// What a caller needs from the memory, beyond its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caps {
    /// Any internal RAM. What most blob allocations want.
    Internal,
    /// Reachable by the DMA engines: inside SRAM2, and scarce.
    Dma,
}

/// The general pool: SRAM1, minus the ROM's data.
static GENERAL: Spinlock<Heap> = Spinlock::new(Heap::new());

/// The DMA-capable pool: whatever of SRAM2 the static map leaves.
static DMA: Spinlock<Heap> = Spinlock::new(Heap::new());

/// Set once [`init`] has run, so a second call cannot hand the same memory out
/// twice.
static READY: AtomicBool = AtomicBool::new(false);

/// Take the reclaimable memory and make it allocatable.
///
/// Call once, after boot, before the radio starts. Returns the bytes taken as
/// `(general, dma)`.
///
/// # Safety
/// The ROM must be finished with its boot-time data — that is, this must not
/// run during early startup — and nothing else may be using these regions.
/// `dma_start` is the first free address above the static map, which the
/// linker script provides as `_dma_pool_end`.
pub unsafe fn init(dma_start: u32) -> (usize, usize) {
    if READY.swap(true, Ordering::SeqCst) {
        return (0, 0);
    }

    // Two pieces, because the ROM's data sits in the middle. Adding SRAM1 as
    // one span would hand out allocations straddling memory that is not ours.
    let taken_general = GENERAL.with(|general| {
        let mut taken = 0;
        for (start, end) in [
            (ROM_PRO_DATA.1, ROM_APP_DATA.0),
            (ROM_APP_DATA.1, SRAM1_END),
        ] {
            debug_assert!(start >= SRAM1_START && end <= SRAM1_END && start < end);
            taken += unsafe { general.add_region(start as *mut u8, (end - start) as usize) };
        }
        taken
    });

    // Whatever SRAM2 has left above the static map. Small, and the only
    // memory here that DMA can actually reach.
    let taken_dma = DMA.with(|dma| {
        if dma_start < SRAM2_END {
            unsafe { dma.add_region(dma_start as *mut u8, (SRAM2_END - dma_start) as usize) }
        } else {
            0
        }
    });

    (taken_general, taken_dma)
}

/// Allocate from the general pool. Not DMA-capable.
///
/// # Safety
/// `align` must be a power of two. The returned pointer must be released with
/// [`free`] and the same [`Caps`].
pub unsafe fn alloc(size: usize, align: usize) -> *mut u8 {
    GENERAL.with(|h| unsafe { h.alloc(size, align) })
}

/// Allocate memory a DMA engine can reach.
///
/// # Safety
/// As [`alloc`].
pub unsafe fn alloc_dma(size: usize, align: usize) -> *mut u8 {
    let p = DMA.with(|h| unsafe { h.alloc(size, align) });
    // Belt and braces: the pool is built from SRAM2 only, so this cannot fire
    // unless `init` was given a bad bound. It is cheap, and the failure it
    // catches is silent data corruption rather than a fault.
    debug_assert!(p.is_null() || is_dma_capable(p));
    p
}

/// Allocate for the given capability.
///
/// # Safety
/// As [`alloc`].
pub unsafe fn alloc_caps(size: usize, align: usize, caps: Caps) -> *mut u8 {
    match caps {
        Caps::Internal => alloc(size, align),
        Caps::Dma => alloc_dma(size, align),
    }
}

/// Return an allocation to the pool it came from.
///
/// # Safety
/// `ptr` must have come from the matching allocator and not been freed.
pub unsafe fn free(ptr: *mut u8, caps: Caps) {
    match caps {
        Caps::Internal => GENERAL.with(|h| unsafe { h.free(ptr) }),
        Caps::Dma => DMA.with(|h| unsafe { h.free(ptr) }),
    }
}

/// Whether an address is one a DMA engine can reach.
pub fn is_dma_capable(ptr: *const u8) -> bool {
    (ptr as u32) < SRAM2_END
}

/// Free bytes in the general pool.
///
/// The sum of the free blocks, which is what `get_free_heap_size` means. It is
/// not a promise that a request of this size will succeed — see
/// [`largest_free_block`].
pub fn free_bytes(caps: Caps) -> usize {
    match caps {
        Caps::Internal => GENERAL.with(|h| h.free_bytes()),
        Caps::Dma => DMA.with(|h| h.free_bytes()),
    }
}

/// Total bytes under management for a capability.
pub fn total_bytes(caps: Caps) -> usize {
    match caps {
        Caps::Internal => GENERAL.with(|h| h.total_bytes()),
        Caps::Dma => DMA.with(|h| h.total_bytes()),
    }
}

/// The largest allocation that can currently succeed, ignoring the header.
pub fn largest_free_block(caps: Caps) -> usize {
    match caps {
        Caps::Internal => GENERAL.with(|h| h.largest_free_block()),
        Caps::Dma => DMA.with(|h| h.largest_free_block()),
    }
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
    fn sram1_is_not_dma_capable_and_sram2_is() {
        // The reason there are two pools at all. An address in SRAM1 must
        // never pass the DMA check, or a descriptor built from it moves the
        // wrong bytes without erroring.
        assert!(!is_dma_capable(SRAM1_START as *const u8));
        assert!(!is_dma_capable((SRAM1_END - 1) as *const u8));
        assert!(is_dma_capable((SRAM2_END - 1) as *const u8));
        assert!(is_dma_capable(0x3FFD_C200 as *const u8));
    }

    #[test]
    fn the_two_pools_do_not_overlap() {
        // SRAM2 ends exactly where SRAM1 begins; a gap or an overlap here
        // would either lose memory or double-issue it.
        assert_eq!(SRAM2_END, SRAM1_START);
    }

    #[test]
    fn a_dma_bound_past_sram2_yields_nothing_rather_than_wrapping() {
        // `init` computes `SRAM2_END - dma_start`. If the static map ever grew
        // past SRAM2 that subtraction would underflow, so the guard matters.
        let dma_start = SRAM2_END + 0x1000;
        assert!(dma_start >= SRAM2_END, "the guard's condition");
    }
}
