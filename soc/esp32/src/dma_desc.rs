// SPDX-License-Identifier: Apache-2.0

//! DMA descriptors. Included by [`crate::dma`].
//!
//! The engine does not take an address and a length. It walks a linked list of
//! 12-byte descriptors, each pointing at one chunk of a buffer, and the list
//! itself has to live where the engine can reach it — the `next` pointers are
//! followed by hardware, so the chain is as much a DMA buffer as the data is.
//!
//! Layout is from the ROM header `esp32/rom/lldesc.h`, whose bitfield starts
//! at the LSB:
//!
//! ```c
//! volatile uint32_t size  :12,   // capacity of the buffer
//!                   length:12,   // valid bytes: TX in, RX out
//!                   offset: 5,
//!                   sosf  : 1,   // start of sub-frame
//!                   eof   : 1,   // last descriptor of the transfer
//!                   owner : 1;   // 1 = engine, 0 = CPU
//! volatile uint8_t *buf;
//! union { volatile uint32_t empty; STAILQ_ENTRY(lldesc_s) qe; };
//! ```
//!
//! The two 12-bit fields are worth stating plainly, because swapping them
//! produces a transfer that runs, reports success, and moves nothing. `size`
//! is how big the buffer is; `length` is how much of it matters. On transmit
//! the CPU sets both and the engine reads `length`. On receive the CPU sets
//! `size` and the **engine writes** `length` with what it actually got.

use super::DmaError;

/// Bit positions in the first descriptor word.
const SIZE_SHIFT: u32 = 0;
const LENGTH_SHIFT: u32 = 12;
const FIELD_MASK: u32 = 0xFFF;
const EOF_BIT: u32 = 1 << 30;
const OWNER_BIT: u32 = 1 << 31;

/// One DMA descriptor.
///
/// `repr(C)` and word-aligned because the engine reads this out of memory. The
/// field order is a hardware contract, not a Rust detail.
#[repr(C, align(4))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    flags: u32,
    buf: u32,
    next: u32,
}

impl Descriptor {
    /// Most bytes one descriptor may carry.
    ///
    /// The `size` field is 12 bits, so 4095 would fit. esp-idf uses 4096-4 and
    /// so do we: a chunk that is not a multiple of 4 leaves the *next* chunk's
    /// buffer address misaligned, and a misaligned DMA address does not fault.
    /// It transfers the wrong bytes.
    pub const MAX_LEN: u32 = 4096 - 4;

    /// An empty descriptor owned by the CPU. What a freshly reserved slot
    /// should look like before a chain is laid over it.
    pub const fn zeroed() -> Self {
        Self { flags: 0, buf: 0, next: 0 }
    }

    /// A transmit descriptor: `len` bytes at `buf`, engine-owned.
    ///
    /// `size` and `length` both get `len`. The engine only reads `length` on
    /// transmit, but leaving `size` at zero describes a buffer too small to
    /// hold what the descriptor claims to send, and costs nothing to get right.
    pub fn tx(buf: u32, len: u32, eof: bool, next: u32) -> Result<Self, DmaError> {
        Self::build(buf, len, len, eof, next)
    }

    /// A receive descriptor: room for `capacity` bytes at `buf`.
    ///
    /// `length` starts at zero because the engine writes it. A receive
    /// descriptor that pre-sets `length` reads back as though it had already
    /// received that much.
    pub fn rx(buf: u32, capacity: u32, eof: bool, next: u32) -> Result<Self, DmaError> {
        Self::build(buf, capacity, 0, eof, next)
    }

    fn build(buf: u32, size: u32, length: u32, eof: bool, next: u32) -> Result<Self, DmaError> {
        if size > Self::MAX_LEN || length > Self::MAX_LEN {
            return Err(DmaError::ChunkTooLong);
        }
        if buf % 4 != 0 || !reachable(buf) {
            return Err(DmaError::UnreachableAddress);
        }
        if next != 0 && (next % 4 != 0 || !reachable(next)) {
            return Err(DmaError::UnreachableAddress);
        }
        Ok(Self {
            flags: ((size & FIELD_MASK) << SIZE_SHIFT)
                | ((length & FIELD_MASK) << LENGTH_SHIFT)
                | if eof { EOF_BIT } else { 0 }
                | OWNER_BIT,
            buf,
            next,
        })
    }

    /// Buffer capacity this descriptor describes.
    pub const fn size(&self) -> u32 {
        (self.flags >> SIZE_SHIFT) & FIELD_MASK
    }

    /// Valid bytes: what the CPU asked to send, or what the engine received.
    pub const fn length(&self) -> u32 {
        (self.flags >> LENGTH_SHIFT) & FIELD_MASK
    }

    /// Last descriptor of a transfer.
    pub const fn is_eof(&self) -> bool {
        self.flags & EOF_BIT != 0
    }

    /// Still owned by the engine.
    ///
    /// Hardware clears this when it is finished with the descriptor, which is
    /// how a completed transfer is recognised without an interrupt.
    pub const fn owned_by_engine(&self) -> bool {
        self.flags & OWNER_BIT != 0
    }

    /// Address of the next descriptor, or 0 at the end of the chain.
    pub const fn next(&self) -> u32 {
        self.next
    }

    /// Address of this descriptor's buffer.
    pub const fn buffer(&self) -> u32 {
        self.buf
    }

    /// The raw first word, for tests that need to see the encoding.
    #[cfg(test)]
    pub(crate) const fn raw_flags(&self) -> u32 {
        self.flags
    }
}

/// SRAM the DMA engines can reach: **all internal DRAM**, `0x3FFAE000` up to
/// but not including `0x40000000`.
///
/// This used to stop at `0x3FFDFFFF`, the end of SRAM2, on the belief that
/// SRAM1 was out of reach. It is not, and esp-idf says so plainly:
///
/// ```c
/// #define SOC_DMA_LOW  0x3FFAE000
/// #define SOC_DMA_HIGH 0x40000000
///
/// inline static bool IRAM_ATTR esp_ptr_dma_capable(const void *p)
/// {
///     return (intptr_t)p >= SOC_DMA_LOW && (intptr_t)p < SOC_DMA_HIGH;
/// }
/// ```
///
/// Its heap agrees: the SRAM1 regions are type 1, whose capability list is
/// `MALLOC_CAP_DMA|MALLOC_CAP_8BIT|MALLOC_CAP_INTERNAL|MALLOC_CAP_DEFAULT`.
/// So does NuttX, which puts ordinary heap regions at `0x3ffe0450` onward.
///
/// The old bound was safe — it rejected only valid memory, never accepted bad
/// — but it cost the radio heap 126 KiB of DMA-capable RAM and would have had
/// the adapter squeezing into 16 KiB for no reason.
///
/// A buffer outside this does not fault. The transfer completes, reports
/// success, and moves nothing — so every address goes through here before it
/// reaches a descriptor.
pub const fn reachable(addr: u32) -> bool {
    addr >= 0x3FFA_E000 && addr < 0x4000_0000
}

/// The link registers hold 20 bits of descriptor address.
///
/// `SPI_OUTLINK_ADDR` and `SPI_INLINK_ADDR` are both `[19:0]`; the top bits
/// are implied. Every address in the reachable region shares them, so this can
/// only fail for an address that had no business being a descriptor.
pub const fn link_addr(addr: u32) -> u32 {
    addr & 0x000F_FFFF
}

/// How many descriptors a buffer of `len` bytes needs.
///
/// Zero-length is one descriptor, not none: the engine still needs something
/// to mark end-of-frame with.
pub const fn descriptors_needed(len: u32) -> u32 {
    if len == 0 {
        1
    } else {
        len.div_ceil(Descriptor::MAX_LEN)
    }
}

/// Which way a chain moves data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Memory to peripheral. The engine reads `length` bytes out.
    Transmit,
    /// Peripheral to memory. The engine fills the buffer and writes `length`.
    Receive,
}

/// Lay a chain over `buf`, writing into `descs`.
///
/// `descs` must itself live in DMA-reachable memory — the engine follows the
/// `next` pointers. Callers get that from `kernel::dma_broker`.
///
/// Returns the address to program into the link register.
///
/// # Safety
/// `descs` must not be in use by a running transfer, and `buf` must stay valid
/// and untouched until this one completes.
pub unsafe fn build_chain(
    descs: &mut [Descriptor],
    buf: u32,
    len: u32,
    direction: Direction,
) -> Result<u32, DmaError> {
    let needed = descriptors_needed(len) as usize;
    if descs.len() < needed {
        return Err(DmaError::NotEnoughDescriptors);
    }
    let head = descs.as_ptr() as u32;
    if head % 4 != 0 || !reachable(head) {
        return Err(DmaError::UnreachableAddress);
    }

    let stride = core::mem::size_of::<Descriptor>() as u32;
    let mut remaining = len;
    for (i, slot) in descs.iter_mut().enumerate().take(needed) {
        let chunk = remaining.min(Descriptor::MAX_LEN);
        let last = i + 1 == needed;
        // Zero terminates the chain; otherwise point at the following slot.
        let next = if last { 0 } else { head + (i as u32 + 1) * stride };
        let offset = len - remaining;
        let desc = match direction {
            Direction::Transmit => Descriptor::tx(buf + offset, chunk, last, next)?,
            Direction::Receive => Descriptor::rx(buf + offset, chunk, last, next)?,
        };
        // Volatile, not `*slot = desc`. The DMA engine reads these descriptors
        // and, in flight, clears the owner bit the CPU set — a write the
        // compiler cannot see. A second transfer that rebuilds a byte-identical
        // descriptor would otherwise be elided as a dead store, leaving the
        // owner bit cleared, and the engine would move nothing. The C lldesc
        // fields are `volatile` for this exact reason.
        core::ptr::write_volatile(slot as *mut Descriptor, desc);
        remaining -= chunk;
    }
    Ok(head)
}

/// Address of the `next` descriptor in a *ring* of `count` slots.
///
/// The one bit of arithmetic unique to a circular chain: slot `count - 1` wraps
/// back to the head instead of terminating at zero. Kept separate so the wrap is
/// host-testable without a DMA-reachable buffer to build over.
const fn ring_next(head: u32, i: usize, count: usize, stride: u32) -> u32 {
    head + (((i + 1) % count) as u32) * stride
}

/// Slot index a descriptor address falls on within a ring whose head is `head`.
///
/// The engine reports the descriptor whose EOF just fired in `IN_EOF_DES_ADDR`;
/// this turns that back into a buffer index. Descriptors are [`Descriptor`]s, so
/// the stride is `size_of::<Descriptor>()`.
pub const fn ring_slot(head: u32, desc_addr: u32) -> usize {
    ((desc_addr - head) / core::mem::size_of::<Descriptor>() as u32) as usize
}

/// Lay a **circular** chain of one-buffer descriptors over a backing buffer
/// split into `count` equal `chunk`-byte buffers, writing into `descs`.
///
/// Unlike [`build_chain`], which builds a linear chain that terminates at
/// end-of-frame, this builds the ring a continuous stream needs: every
/// descriptor is marked EOF — so the engine raises `IN_SUC_EOF` at each buffer
/// boundary — and the last descriptor's `next` points back at the first, so the
/// engine cycles the buffers forever with no CPU intervention to keep it running.
/// esp-idf's `i2s_alloc_dma_buffer` (`driver/i2s.c`) builds exactly this shape:
/// `owner`/`eof` set on every descriptor and the last descriptor's `empty` (its
/// `next` link) pointing back at `desc[0]`.
///
/// `chunk` must be word-aligned and at most [`Descriptor::MAX_LEN`]; `descs` must
/// hold at least `count`. Returns the head address for the link register.
///
/// # Safety
/// Same contract as [`build_chain`]: `descs` must not be in use by a running
/// transfer, and the backing buffer must stay valid for the life of the stream.
pub unsafe fn build_ring(
    descs: &mut [Descriptor],
    buf: u32,
    count: usize,
    chunk: u32,
    direction: Direction,
) -> Result<u32, DmaError> {
    if count == 0 || descs.len() < count {
        return Err(DmaError::NotEnoughDescriptors);
    }
    if chunk > Descriptor::MAX_LEN {
        return Err(DmaError::ChunkTooLong);
    }
    // A chunk that is not a multiple of 4 leaves every buffer after the first
    // misaligned, and a misaligned DMA address moves the wrong bytes silently.
    if chunk % 4 != 0 {
        return Err(DmaError::UnreachableAddress);
    }
    let head = descs.as_ptr() as u32;
    if head % 4 != 0 || !reachable(head) {
        return Err(DmaError::UnreachableAddress);
    }

    let stride = core::mem::size_of::<Descriptor>() as u32;
    for (i, slot) in descs.iter_mut().enumerate().take(count) {
        let next = ring_next(head, i, count, stride);
        let addr = buf + i as u32 * chunk;
        let desc = match direction {
            Direction::Transmit => Descriptor::tx(addr, chunk, true, next)?,
            Direction::Receive => Descriptor::rx(addr, chunk, true, next)?,
        };
        // Volatile for the same reason as `build_chain`: the engine clears the
        // owner bit in flight, and a rebuilt byte-identical descriptor would
        // otherwise be elided as a dead store.
        core::ptr::write_volatile(slot as *mut Descriptor, desc);
    }
    Ok(head)
}

/// Total bytes the engine reported receiving, stopping at end-of-frame.
///
/// Reads the `length` hardware wrote, not the capacity the CPU asked for. A
/// short read is normal and is the number the caller wants.
pub fn received_len(descs: &[Descriptor]) -> u32 {
    let mut total = 0;
    for d in descs {
        total += d.length();
        if d.is_eof() {
            break;
        }
    }
    total
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A word-aligned address inside the reachable region, for tests that only
    /// need somewhere plausible to point.
    const BUF: u32 = 0x3FFD_9000;
    const DESCS: u32 = 0x3FFD_A000;

    #[test]
    fn the_descriptor_is_twelve_bytes_and_word_aligned() {
        // The engine indexes these by stride. A padded struct walks the chain
        // into the gaps between descriptors.
        assert_eq!(core::mem::size_of::<Descriptor>(), 12);
        assert_eq!(core::mem::align_of::<Descriptor>(), 4);
    }

    #[test]
    fn size_and_length_occupy_the_bits_the_rom_header_says() {
        let d = Descriptor::tx(BUF, 0x123, true, 0).unwrap();
        let raw = d.raw_flags();
        assert_eq!(raw & 0xFFF, 0x123, "size is not in bits 0..12");
        assert_eq!((raw >> 12) & 0xFFF, 0x123, "length is not in bits 12..24");
        assert_eq!(raw & (1 << 30), 1 << 30, "eof is not bit 30");
        assert_eq!(raw & (1 << 31), 1 << 31, "owner is not bit 31");
    }

    #[test]
    fn a_receive_descriptor_starts_with_no_received_bytes() {
        let d = Descriptor::rx(BUF, 256, true, 0).unwrap();
        assert_eq!(d.size(), 256, "capacity was not recorded");
        assert_eq!(d.length(), 0, "a fresh rx descriptor claims received bytes");
        assert!(d.owned_by_engine());
    }

    #[test]
    fn a_transmit_descriptor_carries_its_length() {
        let d = Descriptor::tx(BUF, 64, true, 0).unwrap();
        assert_eq!(d.length(), 64);
        assert_eq!(d.size(), 64, "size left short of the length it claims to send");
    }

    #[test]
    fn a_chunk_longer_than_the_field_is_refused() {
        // 4095 fits in 12 bits but breaks word alignment for the next chunk.
        assert_eq!(
            Descriptor::tx(BUF, Descriptor::MAX_LEN + 1, true, 0).unwrap_err(),
            DmaError::ChunkTooLong
        );
        assert!(Descriptor::tx(BUF, Descriptor::MAX_LEN, true, 0).is_ok());
    }

    #[test]
    fn an_unreachable_buffer_is_refused() {
        // The failure this check exists for is silent: the transfer completes
        // and moves nothing. Flash-mapped, RTC, and just past the end.
        for addr in [0x4008_0000, 0x3FF4_0000, 0x3FFA_DFFC, 0x4000_0000] {
            assert_eq!(
                Descriptor::tx(addr, 16, true, 0).unwrap_err(),
                DmaError::UnreachableAddress,
                "{addr:#x} was accepted"
            );
        }
        assert!(Descriptor::tx(0x3FFA_E000, 16, true, 0).is_ok(), "start of DRAM rejected");
        // SRAM1. Rejected until esp-idf was checked: `SOC_DMA_HIGH` is
        // 0x40000000, not the end of SRAM2, and its heap hands out these
        // addresses for MALLOC_CAP_DMA.
        assert!(Descriptor::tx(0x3FFE_0000, 16, true, 0).is_ok(), "start of SRAM1 rejected");
        assert!(Descriptor::tx(0x3FFF_FFFC, 16, true, 0).is_ok(), "end of SRAM1 rejected");
    }

    #[test]
    fn a_misaligned_buffer_is_refused() {
        for addr in [BUF + 1, BUF + 2, BUF + 3] {
            assert_eq!(
                Descriptor::tx(addr, 16, true, 0).unwrap_err(),
                DmaError::UnreachableAddress,
                "{addr:#x} was accepted"
            );
        }
    }

    #[test]
    fn an_unreachable_next_pointer_is_refused() {
        // A bad `next` is worse than a bad buffer: the engine follows it and
        // reads whatever is there as a descriptor.
        assert_eq!(
            Descriptor::tx(BUF, 16, false, 0x4008_0000).unwrap_err(),
            DmaError::UnreachableAddress
        );
        // Zero is the terminator, not an address, and must stay legal.
        assert!(Descriptor::tx(BUF, 16, true, 0).is_ok());
    }

    #[test]
    fn the_link_register_is_unambiguous_across_the_whole_reachable_region() {
        // `link_addr` keeps 20 bits and the top ones are implied, so widening
        // the region to include SRAM1 is only sound if every reachable address
        // still shares those bits. 0x3FFAE000 and 0x3FFFFFFC both sit in the
        // 0x3FF00000 megabyte, so they do.
        assert_eq!(0x3FFA_E000u32 & 0xFFF0_0000, 0x3FF0_0000);
        assert_eq!(0x3FFF_FFFCu32 & 0xFFF0_0000, 0x3FF0_0000);
        assert!(reachable(0x3FFE_0000), "SRAM1 must be reachable");
        assert!(!reachable(0x4000_0000), "past the top of DRAM must not be");
    }

    #[test]
    fn descriptor_counts_cover_the_boundaries() {
        assert_eq!(descriptors_needed(0), 1, "zero length still needs an eof");
        assert_eq!(descriptors_needed(1), 1);
        assert_eq!(descriptors_needed(Descriptor::MAX_LEN), 1);
        assert_eq!(descriptors_needed(Descriptor::MAX_LEN + 1), 2);
        assert_eq!(descriptors_needed(2 * Descriptor::MAX_LEN), 2);
        assert_eq!(descriptors_needed(2 * Descriptor::MAX_LEN + 1), 3);
    }

    /// Build a chain in a real (host) array and check the shape.
    ///
    /// The addresses are host addresses, so `build_chain`'s reachability check
    /// would reject them. These tests drive the per-descriptor constructors
    /// with target-shaped addresses instead, which is what the encoding
    /// actually depends on.
    #[test]
    fn a_chain_links_each_descriptor_to_the_next_and_ends_at_zero() {
        let stride = core::mem::size_of::<Descriptor>() as u32;
        let n = 3u32;
        let mut chain = [Descriptor::zeroed(); 3];
        for i in 0..n {
            let last = i + 1 == n;
            let next = if last { 0 } else { DESCS + (i + 1) * stride };
            chain[i as usize] =
                Descriptor::tx(BUF + i * Descriptor::MAX_LEN, Descriptor::MAX_LEN, last, next)
                    .unwrap();
        }
        assert_eq!(chain[0].next(), DESCS + stride);
        assert_eq!(chain[1].next(), DESCS + 2 * stride);
        assert_eq!(chain[2].next(), 0, "the chain does not terminate");
        assert!(!chain[0].is_eof());
        assert!(!chain[1].is_eof());
        assert!(chain[2].is_eof(), "the last descriptor is not marked eof");
    }

    #[test]
    fn received_length_sums_to_the_end_of_frame_and_no_further() {
        // Descriptors past eof belong to no transfer. Counting them inflates
        // the reported length by whatever the previous transfer left behind.
        let a = Descriptor::rx(BUF, 100, false, DESCS).unwrap();
        let mut b = Descriptor::rx(BUF + 100, 100, true, 0).unwrap();
        let stale = Descriptor::tx(BUF + 200, 999, true, 0).unwrap();

        // Hardware would write these; forge them by rebuilding with a length.
        let a = Descriptor::tx(a.buffer(), 100, false, a.next()).unwrap();
        b = Descriptor::tx(b.buffer(), 40, true, 0).unwrap();
        assert_eq!(received_len(&[a, b, stale]), 140);
    }

    #[test]
    fn a_ring_next_pointer_wraps_the_last_slot_back_to_the_head() {
        let stride = core::mem::size_of::<Descriptor>() as u32;
        // Four slots: 0->1->2->3->0. Anything but the wrap is a linear chain.
        assert_eq!(ring_next(DESCS, 0, 4, stride), DESCS + stride);
        assert_eq!(ring_next(DESCS, 2, 4, stride), DESCS + 3 * stride);
        assert_eq!(ring_next(DESCS, 3, 4, stride), DESCS, "the ring does not close");
        // A one-slot ring points at itself, not off the end.
        assert_eq!(ring_next(DESCS, 0, 1, stride), DESCS);
    }

    #[test]
    fn a_ring_slot_recovers_the_index_from_a_descriptor_address() {
        let stride = core::mem::size_of::<Descriptor>() as u32;
        // The inverse of `ring_next`/the buffer layout: IN_EOF_DES_ADDR back to
        // a buffer index.
        assert_eq!(ring_slot(DESCS, DESCS), 0);
        assert_eq!(ring_slot(DESCS, DESCS + stride), 1);
        assert_eq!(ring_slot(DESCS, DESCS + 3 * stride), 3);
    }

    #[test]
    fn a_ring_of_target_descriptors_is_circular_and_all_eof() {
        // Drive the ring shape through the real constructors with target-shaped
        // addresses (as the other chain tests do — `build_ring`'s reachability
        // check rejects host addresses).
        let stride = core::mem::size_of::<Descriptor>() as u32;
        let count = 4usize;
        let chunk = 64u32;
        let mut ring = [Descriptor::zeroed(); 4];
        for i in 0..count {
            let next = ring_next(DESCS, i, count, stride);
            ring[i] = Descriptor::rx(BUF + i as u32 * chunk, chunk, true, next).unwrap();
        }
        for (i, d) in ring.iter().enumerate() {
            assert!(d.is_eof(), "slot {i} is not eof; the engine will not signal a boundary");
            assert!(d.owned_by_engine());
            assert_ne!(d.next(), 0, "a ring descriptor must never terminate");
        }
        assert_eq!(ring[3].next(), DESCS, "the ring does not close on itself");
    }

    #[test]
    fn build_ring_rejects_a_misaligned_or_oversized_chunk() {
        // These fail before any reachability check, so host addresses are fine.
        let mut descs = [Descriptor::zeroed(); 2];
        // Not a multiple of 4.
        assert_eq!(
            unsafe { build_ring(&mut descs, 0x3FFD_9000, 2, 60 + 2, Direction::Transmit) },
            Err(DmaError::UnreachableAddress)
        );
        // Bigger than one descriptor can carry.
        assert_eq!(
            unsafe { build_ring(&mut descs, 0x3FFD_9000, 2, Descriptor::MAX_LEN + 4, Direction::Receive) },
            Err(DmaError::ChunkTooLong)
        );
        // Too few descriptors for the requested buffer count.
        assert_eq!(
            unsafe { build_ring(&mut descs, 0x3FFD_9000, 3, 64, Direction::Receive) },
            Err(DmaError::NotEnoughDescriptors)
        );
    }

    #[test]
    fn the_link_register_keeps_twenty_bits_of_the_address() {
        // SPI_OUTLINK_ADDR is [19:0]; the top bits are implied. Programming
        // the full address instead sets the start/stop bits that share the
        // register, which starts a transfer nobody asked for.
        assert_eq!(link_addr(0x3FFD_9000), 0xD9000);
        assert_eq!(link_addr(0x3FFA_E000), 0xAE000);
        assert!(link_addr(0x3FFD_9000) & !0x000F_FFFF == 0);
    }
}
