// SPDX-License-Identifier: Apache-2.0

//! A free-list allocator over regions the caller supplies.
//!
//! FlintOS has no allocator, deliberately: static allocation is what gives an
//! RTOS bounded latency and no fragmentation. This exists for one reason —
//! Espressif's radio blobs allocate constantly and cannot be linked without a
//! heap — and it is meant to stay confined to them. Nothing in the kernel
//! should call it.
//!
//! # Not a `GlobalAlloc`
//!
//! Deliberately. A `#[global_allocator]` would make `alloc::` available
//! everywhere and turn "the kernel does not allocate" from a property into a
//! convention that erodes. Callers hold a [`Heap`] and allocate from it
//! explicitly, so every allocation site is visible in the source.
//!
//! # Layout
//!
//! Each allocation carries an eight-byte header immediately before the payload:
//!
//! ```text
//!   block start                      payload
//!   |                                |
//!   v                                v
//!   [ .. alignment padding .. ][ hdr ][ user bytes .. ][ .. slack .. ]
//!                               ^^^^^
//!                               total: bytes in the whole block
//!                               back:  distance from block start to hdr
//! ```
//!
//! `back` is what makes over-aligned allocations work without leaking. When
//! aligning the payload leaves a gap too small to be its own free block, the
//! gap is absorbed into the allocation and `back` records how to find the real
//! start again at free time. Both fields are `u32` so the header is eight
//! bytes on the host and on a 32-bit target alike — the tests then exercise
//! the same layout that ships.
//!
//! The free list is singly linked, **ordered by address**, and stored inside
//! the free blocks themselves. Address order is what makes coalescing on free
//! an O(1) check against the two neighbours rather than a scan.

#![no_std]

use core::ptr;

/// Payload alignment floor. Eight bytes, so a `u64` field is legally placed.
pub const ALIGN: usize = 8;

/// Bytes of bookkeeping before each payload.
pub const HEADER: usize = core::mem::size_of::<Header>();

/// Smallest block that can sit on the free list, since a free block stores the
/// list node in its own bytes. Sixteen covers both a 64-bit host and a 32-bit
/// target, so behaviour does not differ between test and target.
pub const MIN_BLOCK: usize = 16;

#[repr(C)]
struct Header {
    /// Bytes in the whole block, measured from the block start.
    total: u32,
    /// Distance from the block start to this header.
    back: u32,
}

#[repr(C)]
struct Free {
    /// Bytes in this free block, including these bookkeeping bytes.
    size: u32,
    next: *mut Free,
}

const fn align_up(v: usize, to: usize) -> usize {
    (v + to - 1) & !(to - 1)
}

/// A heap over one or more regions.
///
/// Not `Sync`: it holds raw pointers and does no locking of its own. A caller
/// sharing one across tasks or cores must wrap it — see `kernel::heap`.
pub struct Heap {
    head: *mut Free,
    total: usize,
    used: usize,
}

impl Heap {
    /// An empty heap. Add regions with [`Heap::add_region`].
    pub const fn new() -> Self {
        Self { head: ptr::null_mut(), total: 0, used: 0 }
    }

    /// Give the heap a region to allocate from.
    ///
    /// Regions need not be contiguous or added in order; disjoint ones are
    /// exactly why the ESP32 needs this, since the ROM's data sits in the
    /// middle of SRAM1 and splits it in two.
    ///
    /// Returns the number of bytes actually taken, which is less than `len`
    /// when the ends need aligning, and zero when what is left is too small to
    /// track.
    ///
    /// # Safety
    /// `start..start + len` must be valid, writable, otherwise unused for the
    /// lifetime of this heap, and must not overlap a region already added.
    pub unsafe fn add_region(&mut self, start: *mut u8, len: usize) -> usize {
        let begin = align_up(start as usize, ALIGN);
        let end = (start as usize + len) & !(ALIGN - 1);
        if end <= begin || end - begin < MIN_BLOCK {
            return 0;
        }
        let size = end - begin;
        self.total += size;
        self.insert_free(begin, size);
        size
    }

    /// Allocate `size` bytes aligned to at least `align`.
    ///
    /// Returns null when the request cannot be met. Null rather than a panic
    /// because the caller is a C blob that checks for it, and because an
    /// allocator that panics on exhaustion is not one an RTOS can ship.
    ///
    /// # Safety
    /// `align` must be a power of two.
    pub unsafe fn alloc(&mut self, size: usize, align: usize) -> *mut u8 {
        debug_assert!(align.is_power_of_two());
        let align = if align < ALIGN { ALIGN } else { align };
        // A zero-byte request still gets a distinct address, as malloc does.
        let size = align_up(if size == 0 { 1 } else { size }, ALIGN);

        let mut prev: *mut *mut Free = &mut self.head;
        while !(*prev).is_null() {
            let block = *prev;
            let block_start = block as usize;
            let block_size = (*block).size as usize;
            let payload = align_up(block_start + HEADER, align);

            if payload + size > block_start + block_size {
                prev = &mut (*block).next;
                continue;
            }

            // Where the allocation begins. A gap in front becomes its own free
            // block when it is big enough to track, and is otherwise absorbed
            // — `back` in the header is what finds the start again.
            let front = payload - HEADER - block_start;
            let alloc_start = if front >= MIN_BLOCK {
                (*block).size = front as u32;
                block_start + front
            } else {
                *prev = (*block).next;
                block_start
            };

            // Slack at the end goes back on the list on the same terms.
            let mut alloc_end = payload + size;
            let tail = (block_start + block_size) - alloc_end;
            if tail >= MIN_BLOCK {
                self.insert_free(alloc_end, tail);
            } else {
                alloc_end = block_start + block_size;
            }

            let header = (payload - HEADER) as *mut Header;
            (*header).total = (alloc_end - alloc_start) as u32;
            (*header).back = ((payload - HEADER) - alloc_start) as u32;
            self.used += alloc_end - alloc_start;
            return payload as *mut u8;
        }
        ptr::null_mut()
    }

    /// Return an allocation to the heap.
    ///
    /// # Safety
    /// `ptr` must have come from [`Heap::alloc`] on this heap and must not
    /// have been freed already.
    pub unsafe fn free(&mut self, ptr: *mut u8) {
        if ptr.is_null() {
            return;
        }
        let header = (ptr as usize - HEADER) as *mut Header;
        let total = (*header).total as usize;
        let start = (ptr as usize - HEADER) - (*header).back as usize;
        self.used -= total;
        self.insert_free(start, total);
    }

    /// Bytes the heap manages in total, across every region.
    pub fn total_bytes(&self) -> usize {
        self.total
    }

    /// Bytes not currently allocated.
    ///
    /// This is the honest figure — the sum of the free blocks — and not a
    /// promise that a request of this size will succeed. Fragmentation means
    /// it usually will not; [`Heap::largest_free_block`] is the number that
    /// answers that question.
    pub fn free_bytes(&self) -> usize {
        self.total - self.used
    }

    /// The largest single free block, in bytes.
    ///
    /// An allocation of `largest_free_block() - HEADER` at default alignment
    /// is the largest that can currently succeed.
    pub fn largest_free_block(&self) -> usize {
        let mut best = 0;
        let mut node = self.head;
        while !node.is_null() {
            // Safety: every node on the list is a live free block.
            let size = unsafe { (*node).size } as usize;
            if size > best {
                best = size;
            }
            node = unsafe { (*node).next };
        }
        best
    }

    /// Insert a block, merging it with whichever neighbours touch it.
    ///
    /// # Safety
    /// `start..start + size` must be owned by this heap and not on the list.
    unsafe fn insert_free(&mut self, start: usize, size: usize) {
        let node = start as *mut Free;
        (*node).size = size as u32;
        (*node).next = ptr::null_mut();

        // Address-ordered insert.
        let mut prev: *mut *mut Free = &mut self.head;
        while !(*prev).is_null() && (*prev as usize) < start {
            prev = &mut (**prev).next;
        }
        (*node).next = *prev;
        *prev = node;

        // Merge forward, then let the predecessor merge into the result. Doing
        // it in this order means a block freed between two free neighbours
        // becomes one block rather than two.
        let next = (*node).next;
        if !next.is_null() && start + size == next as usize {
            (*node).size += (*next).size;
            (*node).next = (*next).next;
        }
        if !core::ptr::eq(prev, &mut self.head as *mut *mut Free) {
            // `prev` points at some block's `next` field; recover that block.
            let before = (prev as usize - core::mem::offset_of!(Free, next)) as *mut Free;
            if before as usize + (*before).size as usize == start {
                (*before).size += (*node).size;
                (*before).next = (*node).next;
            }
        }
    }
}

// The heap owns the regions it was given, and its pointers are only ever
// dereferenced through `&mut self`. Moving one between cores is therefore
// sound provided access is serialised, which is what the caller's lock is for.
// `Sync` is deliberately *not* implemented: sharing one without a lock is not
// sound, and leaving it out means the compiler says so.
unsafe impl Send for Heap {}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// A heap over a leaked, over-aligned buffer.
    ///
    /// Leaked on purpose: the allocations handed out point into it, and the
    /// tests are more legible without a lifetime threaded through every one.
    fn heap_of(bytes: usize) -> (Heap, *mut u8, usize) {
        let buf: Vec<u8> = vec![0; bytes + 64];
        let raw = Vec::leak(buf);
        let start = align_up(raw.as_mut_ptr() as usize, 64) as *mut u8;
        let mut h = Heap::new();
        let taken = unsafe { h.add_region(start, bytes) };
        (h, start, taken)
    }

    #[test]
    fn an_empty_heap_hands_out_nothing() {
        let mut h = Heap::new();
        assert!(unsafe { h.alloc(1, 8) }.is_null());
        assert_eq!(h.total_bytes(), 0);
        assert_eq!(h.free_bytes(), 0);
        assert_eq!(h.largest_free_block(), 0);
    }

    #[test]
    fn a_region_too_small_to_track_is_refused_rather_than_half_added() {
        let mut h = Heap::new();
        let mut tiny = [0u8; 8];
        assert_eq!(unsafe { h.add_region(tiny.as_mut_ptr(), 8) }, 0);
        assert_eq!(h.total_bytes(), 0);
    }

    #[test]
    fn an_allocation_round_trips_and_returns_every_byte() {
        let (mut h, _, taken) = heap_of(1024);
        let before = h.free_bytes();
        assert_eq!(before, taken);

        let p = unsafe { h.alloc(100, 8) };
        assert!(!p.is_null());
        assert!(h.free_bytes() < before);

        unsafe { h.free(p) };
        assert_eq!(h.free_bytes(), before, "freeing must return every byte");
        // And the whole region is one block again, not two touching ones.
        assert_eq!(h.largest_free_block(), taken);
    }

    #[test]
    fn allocations_are_aligned_as_asked() {
        let (mut h, _, _) = heap_of(4096);
        let mut held = Vec::new();
        for align in [8usize, 16, 32, 64, 128] {
            let p = unsafe { h.alloc(24, align) };
            assert!(!p.is_null(), "align {align} failed");
            assert_eq!(p as usize % align, 0, "align {align} not honoured");
            held.push(p);
        }
        for p in held {
            unsafe { h.free(p) };
        }
        assert_eq!(h.free_bytes(), h.total_bytes());
    }

    #[test]
    fn allocations_do_not_overlap_each_other() {
        // Fill each allocation with a distinct byte, then check none of them
        // changed. Overlap is the failure that silently corrupts a blob.
        let (mut h, _, _) = heap_of(4096);
        let mut held = Vec::new();
        for i in 0..16u8 {
            let size = 17 + i as usize * 3; // deliberately unaligned sizes
            let p = unsafe { h.alloc(size, 8) };
            assert!(!p.is_null());
            unsafe { ptr::write_bytes(p, i, size) };
            held.push((p, size, i));
        }
        for &(p, size, i) in &held {
            let seen = unsafe { core::slice::from_raw_parts(p, size) };
            assert!(seen.iter().all(|&b| b == i), "allocation {i} was overwritten");
        }
        for &(p, _, _) in &held {
            unsafe { h.free(p) };
        }
        assert_eq!(h.free_bytes(), h.total_bytes());
    }

    #[test]
    fn a_freed_hole_is_reused() {
        let (mut h, _, _) = heap_of(1024);
        let a = unsafe { h.alloc(64, 8) };
        let b = unsafe { h.alloc(64, 8) };
        let c = unsafe { h.alloc(64, 8) };
        assert!(!a.is_null() && !b.is_null() && !c.is_null());

        unsafe { h.free(b) };
        // Something that fits in the hole should land in it rather than
        // extending into fresh memory.
        let d = unsafe { h.alloc(32, 8) };
        assert_eq!(d, b, "the hole should have been reused");

        unsafe { h.free(a) };
        unsafe { h.free(c) };
        unsafe { h.free(d) };
        assert_eq!(h.free_bytes(), h.total_bytes());
    }

    #[test]
    fn adjacent_frees_coalesce_in_every_order() {
        // The three cases that matter: merging with the block after, with the
        // block before, and with both at once. Freeing the middle one last is
        // what exercises the third.
        for order in [[0, 1, 2], [2, 1, 0], [0, 2, 1]] {
            let (mut h, _, taken) = heap_of(1024);
            let ps: Vec<*mut u8> = (0..3).map(|_| unsafe { h.alloc(64, 8) }).collect();
            assert!(ps.iter().all(|p| !p.is_null()));
            for i in order {
                unsafe { h.free(ps[i]) };
            }
            assert_eq!(h.free_bytes(), taken, "order {order:?}");
            assert_eq!(
                h.largest_free_block(),
                taken,
                "order {order:?} left the heap fragmented"
            );
        }
    }

    #[test]
    fn exhaustion_returns_null_and_leaves_the_heap_usable() {
        let (mut h, _, _) = heap_of(512);
        // Far larger than the region.
        assert!(unsafe { h.alloc(4096, 8) }.is_null());
        // The failed request must not have consumed anything.
        assert_eq!(h.free_bytes(), h.total_bytes());
        // And the heap still works.
        let p = unsafe { h.alloc(64, 8) };
        assert!(!p.is_null());
        unsafe { h.free(p) };
        assert_eq!(h.free_bytes(), h.total_bytes());
    }

    #[test]
    fn allocating_until_empty_then_freeing_restores_the_heap() {
        let (mut h, _, taken) = heap_of(2048);
        let mut held = Vec::new();
        loop {
            let p = unsafe { h.alloc(48, 8) };
            if p.is_null() {
                break;
            }
            held.push(p);
        }
        assert!(!held.is_empty(), "should fit at least one allocation");
        assert!(unsafe { h.alloc(48, 8) }.is_null(), "should be exhausted");
        for p in held {
            unsafe { h.free(p) };
        }
        assert_eq!(h.free_bytes(), taken);
        assert_eq!(h.largest_free_block(), taken);
    }

    #[test]
    fn largest_free_block_tracks_fragmentation() {
        let (mut h, _, taken) = heap_of(1024);
        assert_eq!(h.largest_free_block(), taken);

        let a = unsafe { h.alloc(64, 8) };
        let b = unsafe { h.alloc(64, 8) };
        unsafe { h.free(a) };

        // Plenty free overall, but not in one piece — which is the difference
        // `get_free_heap_size` alone would hide.
        assert!(h.free_bytes() > h.largest_free_block());
        unsafe { h.free(b) };
        assert_eq!(h.largest_free_block(), taken);
    }

    #[test]
    fn disjoint_regions_are_both_used_and_never_merged() {
        // The ESP32 case: ROM data splits SRAM1 in two. A merge across the gap
        // would hand out an allocation straddling memory the heap does not own.
        let mut h = Heap::new();
        let buf: Vec<u8> = vec![0; 4096];
        let raw = Vec::leak(buf);
        let base = align_up(raw.as_mut_ptr() as usize, 64);
        let first = base as *mut u8;
        // A deliberate 512-byte hole between them.
        let second = (base + 1024 + 512) as *mut u8;
        let a = unsafe { h.add_region(first, 1024) };
        let b = unsafe { h.add_region(second, 1024) };
        assert_eq!(h.total_bytes(), a + b);

        // Neither region alone can satisfy something larger than itself, even
        // though the total says there is room.
        assert!(unsafe { h.alloc(1500, 8) }.is_null());
        assert!(h.free_bytes() > 1500);

        // But one allocation from each is fine.
        let p = unsafe { h.alloc(900, 8) };
        let q = unsafe { h.alloc(900, 8) };
        assert!(!p.is_null() && !q.is_null());
        unsafe { h.free(p) };
        unsafe { h.free(q) };
        assert_eq!(h.free_bytes(), h.total_bytes());
    }

    #[test]
    fn a_zero_byte_request_still_gets_its_own_address() {
        let (mut h, _, _) = heap_of(256);
        let a = unsafe { h.alloc(0, 8) };
        let b = unsafe { h.alloc(0, 8) };
        assert!(!a.is_null() && !b.is_null());
        assert_ne!(a, b);
        unsafe { h.free(a) };
        unsafe { h.free(b) };
        assert_eq!(h.free_bytes(), h.total_bytes());
    }

    #[test]
    fn freeing_null_is_a_no_op() {
        let (mut h, _, taken) = heap_of(256);
        unsafe { h.free(ptr::null_mut()) };
        assert_eq!(h.free_bytes(), taken);
    }

    #[test]
    fn a_churn_of_mixed_sizes_does_not_lose_memory() {
        // The property that matters over a long radio session: after any
        // sequence of allocations and frees, everything comes back.
        let (mut h, _, taken) = heap_of(8192);
        let mut held: Vec<*mut u8> = Vec::new();
        // Deterministic pseudo-random churn -- no rand dependency, and a fixed
        // sequence means a failure reproduces.
        let mut state = 12345u32;
        let mut next = || {
            state = state.wrapping_mul(1103515245).wrapping_add(12345);
            (state >> 16) as usize
        };
        for _ in 0..2000 {
            if held.is_empty() || next() % 3 != 0 {
                let size = 8 + next() % 200;
                let align = 1 << (3 + next() % 3); // 8, 16 or 32
                let p = unsafe { h.alloc(size, align) };
                if !p.is_null() {
                    assert_eq!(p as usize % align, 0);
                    unsafe { ptr::write_bytes(p, 0xAB, size) };
                    held.push(p);
                }
            } else {
                let i = next() % held.len();
                unsafe { h.free(held.swap_remove(i)) };
            }
        }
        for p in held {
            unsafe { h.free(p) };
        }
        assert_eq!(h.free_bytes(), taken, "churn lost memory");
        assert_eq!(h.largest_free_block(), taken, "churn left the heap fragmented");
    }
}
