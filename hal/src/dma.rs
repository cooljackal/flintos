// SPDX-License-Identifier: Apache-2.0

//! DMA reachability — the one fact a driver needs before pointing an engine at
//! a buffer.
//!
//! A DMA engine reaches only certain address ranges: on the ESP32 that is
//! internal DRAM (`0x3FFAE000..0x40000000`), on a Cortex-M it is a different
//! SRAM window, and external PSRAM or flash-mapped memory is off-limits to
//! both. A driver that starts a transfer into an unreachable buffer gets
//! silence or garbage, not a fault — so the check has to happen first.
//!
//! [`DmaReach`] is that check, behind a trait so the rule lives with the SoC
//! and a portable driver does not hard-code one chip's memory map. It is the
//! first of the `hal` seams the second-architecture plan calls for
//! (`doc/plan-arm32.md`, Phase 1.1).

/// Why the kernel's DMA broker refused or lost a transfer.
///
/// Defined here rather than in the kernel so a driver can name it without
/// naming the kernel; the kernel re-exports it from `dma_broker`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    /// Requested size does not fit the remaining pool.
    PoolExhausted,
    /// Caller does not own the handle.
    NotOwner,
    /// The transfer did not complete in time.
    Timeout,
}

impl core::fmt::Display for DmaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PoolExhausted => f.write_str("DMA pool exhausted"),
            Self::NotOwner => f.write_str("not the owner of this DMA buffer"),
            Self::Timeout => f.write_str("DMA transfer timed out"),
        }
    }
}

/// Whether a byte range is reachable by this SoC's DMA engines.
pub trait DmaReach: Send + Sync {
    /// Whether every byte of `[addr, addr + len)` lies in DMA-reachable memory.
    ///
    /// `len == 0` is vacuously reachable. An implementation must account for the
    /// end of the range, not just its start: a buffer that begins in reachable
    /// memory and runs off the end of it is not reachable.
    fn reachable(&self, addr: u32, len: u32) -> bool;
}

/// Whether every byte of `[addr, addr + len)` lies within `[low, high)`.
///
/// The shared building block for [`DmaReach::reachable`] impls whose reachable
/// memory is a single contiguous window: only `low`/`high` differ per chip.
/// `len == 0` is vacuously within; a range whose last byte would overflow
/// `u32` is rejected rather than wrapped.
pub const fn range_within(addr: u32, len: u32, low: u32, high: u32) -> bool {
    if len == 0 {
        return true;
    }
    match addr.checked_add(len - 1) {
        Some(last) => addr >= low && last < high,
        None => false,
    }
}

/// Identifies one transfer, from the broker's `begin` to its `await_transfer`.
///
/// Lives here rather than in the kernel so a driver — and `api` — can name it
/// without naming the kernel; the kernel re-exports it from `dma_broker`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaTransferId(u32);

impl DmaTransferId {
    /// The raw id, for a driver that must stash it somewhere an interrupt
    /// handler can reach — an `AtomicU32`, typically, since the top-half
    /// cannot take a lock.
    pub const fn raw(&self) -> u32 {
        self.0
    }

    /// Rebuild an id from [`DmaTransferId::raw`].
    ///
    /// Zero is not a valid id: the broker counts from one, so a driver can use
    /// it to mean "nothing in flight".
    pub const fn from_raw(v: u32) -> Self {
        Self(v)
    }
}

/// A handle to a DMA-safe buffer the broker handed out.
///
/// Lives here, not in the kernel, so a driver and `api` can name it without
/// naming the kernel; the kernel's broker builds it with [`DmaHandle::new`]
/// and re-exports the type from `dma_broker`.
///
/// It carries the buffer's already-resolved address rather than an offset: the
/// pool base is a linker symbol only the kernel can see, so the kernel does the
/// arithmetic once, at allocation, and every reader here just reads the answer.
#[derive(Debug, Clone, Copy)]
pub struct DmaHandle {
    addr: u32,
    size: u32,
    owner_task: u32,
}

impl DmaHandle {
    /// Build a handle over an already-resolved buffer. **Broker only** — the
    /// address, size and owner are the broker's invariants (in the DMA pool,
    /// alignment-rounded, owned by the allocating task), and a handle minted
    /// anywhere else asserts them without having earned them.
    pub const fn new(addr: u32, size: u32, owner_task: u32) -> Self {
        Self {
            addr,
            size,
            owner_task,
        }
    }

    /// The buffer's address, for programming into a DMA engine.
    ///
    /// Guaranteed by the broker to be inside the DMA pool and 4-byte aligned: a
    /// descriptor or buffer that is neither is the failure this type exists to
    /// prevent. A misaligned address does not fault — the engine transfers the
    /// wrong bytes.
    pub const fn addr(&self) -> u32 {
        self.addr
    }

    /// Size of this buffer in bytes, rounded up to the pool's alignment.
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// Task that allocated this buffer, and the only one permitted to use it.
    pub const fn owner_task(&self) -> u32 {
        self.owner_task
    }
}
