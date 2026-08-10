// SPDX-License-Identifier: Apache-2.0

//! Radio-heap self-tests. Included by [`crate::selftest`].
//!
//! The allocator itself is tested on the host, thoroughly and against an
//! ordinary buffer — fragmentation, coalescing, exhaustion, a two-thousand
//! operation churn. None of that needs a chip.
//!
//! What needs a chip is the premise underneath it: that the memory
//! `kernel::heap` hands out is **real, writable RAM that nothing else owns**.
//! That claim rests on a comment in the linker script —
//!
//! > Memory above the bound is reclaimable as heap once the ROM is finished
//!
//! — and on two addresses copied out of esp-idf for the ROM's own data. If any
//! of that is wrong, the host tests still pass and the board corrupts itself
//! the moment the radio starts. A write that lands nowhere, or on top of the
//! ROM's data, is exactly the kind of failure that shows up much later and
//! somewhere else.
//!
//! So these tests write patterns through the allocator and read them back off
//! the hardware.

use crate::heap::{self, Caps};

use super::Check;

/// Sizes chosen to cross a page and to be awkward rather than round.
const PATTERN_LEN: usize = 1024 + 24;

/// `init` must produce a heap worth having.
///
/// Runs first, because everything below depends on it. Calling it twice is
/// harmless — the second call returns zero — so the ordering of the suite does
/// not have to be defended.
pub(super) fn reclaimed_memory_is_available() -> Check {
    // `_dma_pool_end` is the first address above the static map. Taking it from
    // the linker rather than repeating a constant means the map can move --
    // and it does, under `radio-bt` -- without this going stale.
    unsafe extern "C" {
        static _dma_pool_end: u8;
    }
    let dma_start = (&raw const _dma_pool_end) as u32;
    let (general, dma) = unsafe { heap::init(dma_start) };

    // A second call must not hand the same memory out again.
    let (again_general, again_dma) = unsafe { heap::init(dma_start) };
    if again_general != 0 || again_dma != 0 {
        return Err("init handed out memory twice");
    }

    // Wi-Fi wants roughly 50 KB from the general pool. Less than that and the
    // radio plan does not fit, which is worth knowing here rather than in
    // phase 5.
    if general < 100 * 1024 {
        return Err("general pool is smaller than the radio needs");
    }
    if dma == 0 {
        return Err("no DMA-capable memory was reclaimed");
    }
    if heap::total_bytes(Caps::Internal) != general {
        return Err("general total disagrees with what init reported");
    }
    if heap::total_bytes(Caps::Dma) != dma {
        return Err("dma total disagrees with what init reported");
    }
    Ok(())
}

/// Memory from the general pool must hold what is written to it.
///
/// The one that catches a region that is not really there: a write to an
/// unbacked address on this part does not fault, it is simply lost.
pub(super) fn general_memory_holds_a_pattern() -> Check {
    let p = unsafe { heap::alloc(PATTERN_LEN, 8) };
    if p.is_null() {
        return Err("general allocation failed");
    }
    // A varying pattern, not a constant: a stuck bus returns the same byte
    // everywhere and would pass a memset-and-compare.
    for i in 0..PATTERN_LEN {
        unsafe { p.add(i).write_volatile((i as u8) ^ 0x5A) };
    }
    for i in 0..PATTERN_LEN {
        if unsafe { p.add(i).read_volatile() } != (i as u8) ^ 0x5A {
            unsafe { heap::free(p, Caps::Internal) };
            return Err("general memory did not hold what was written");
        }
    }
    unsafe { heap::free(p, Caps::Internal) };
    Ok(())
}

/// Two live allocations must not overlap, on the chip as on the host.
pub(super) fn two_allocations_do_not_overlap() -> Check {
    let a = unsafe { heap::alloc(512, 8) };
    let b = unsafe { heap::alloc(512, 8) };
    if a.is_null() || b.is_null() {
        return Err("allocation failed");
    }
    unsafe {
        for i in 0..512 {
            a.add(i).write_volatile(0xAA);
            b.add(i).write_volatile(0x55);
        }
    }
    let mut bad = false;
    unsafe {
        for i in 0..512 {
            if a.add(i).read_volatile() != 0xAA || b.add(i).read_volatile() != 0x55 {
                bad = true;
            }
        }
        heap::free(a, Caps::Internal);
        heap::free(b, Caps::Internal);
    }
    if bad {
        return Err("allocations overlapped");
    }
    Ok(())
}

/// DMA-capable memory must be addressable by the DMA engines.
///
/// SRAM1 is the big region and the engines cannot reach it. A descriptor built
/// from an SRAM1 address does not error — the transfer moves the wrong bytes —
/// so this is checked rather than assumed.
pub(super) fn dma_memory_is_where_dma_can_reach() -> Check {
    let p = unsafe { heap::alloc_dma(256, 4) };
    if p.is_null() {
        return Err("DMA allocation failed");
    }
    if !heap::is_dma_capable(p) {
        unsafe { heap::free(p, Caps::Dma) };
        return Err("DMA pool returned memory outside SRAM2");
    }
    for i in 0..256 {
        unsafe { p.add(i).write_volatile(i as u8) };
    }
    for i in 0..256 {
        if unsafe { p.add(i).read_volatile() } != i as u8 {
            unsafe { heap::free(p, Caps::Dma) };
            return Err("DMA memory did not hold what was written");
        }
    }
    unsafe { heap::free(p, Caps::Dma) };
    Ok(())
}

/// The general pool must not be handing out DMA-reachable memory by accident.
///
/// If it did, the distinction between the two pools would be untested luck,
/// and a caller could get away with `Caps::Internal` until the day it moved.
pub(super) fn general_memory_is_not_in_the_dma_region() -> Check {
    let p = unsafe { heap::alloc(64, 8) };
    if p.is_null() {
        return Err("general allocation failed");
    }
    let overlaps = heap::is_dma_capable(p);
    unsafe { heap::free(p, Caps::Internal) };
    if overlaps {
        return Err("general pool overlaps the DMA region");
    }
    Ok(())
}

/// Every byte must come back, and exhaustion must be reported honestly.
pub(super) fn the_pool_returns_to_full_after_use() -> Check {
    let before = heap::free_bytes(Caps::Internal);

    // A request larger than the whole pool must fail rather than wrap or
    // return something unusable.
    let huge = unsafe { heap::alloc(usize::MAX / 2, 8) };
    if !huge.is_null() {
        return Err("an impossible allocation succeeded");
    }
    if heap::free_bytes(Caps::Internal) != before {
        return Err("a failed allocation consumed memory");
    }

    let mut held = [core::ptr::null_mut(); 16];
    for slot in held.iter_mut() {
        *slot = unsafe { heap::alloc(200, 8) };
        if slot.is_null() {
            return Err("allocation failed with memory available");
        }
    }
    if heap::free_bytes(Caps::Internal) >= before {
        return Err("free_bytes did not fall while memory was held");
    }
    for slot in held.iter() {
        unsafe { heap::free(*slot, Caps::Internal) };
    }
    if heap::free_bytes(Caps::Internal) != before {
        return Err("memory was lost across an allocate/free cycle");
    }
    Ok(())
}
