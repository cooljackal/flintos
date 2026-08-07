// SPDX-License-Identifier: Apache-2.0

//! DMA Broker (Phase 3).
//!
//! Manages a pool of DMA-safe SRAM and provides safe submission/await
//! for physical driver tasks.  Drivers never touch DMA engine registers
//! — the broker validates and programs them.

use core::sync::atomic::{AtomicU32, Ordering};

/// DMA transfer direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaDirection {
    Read,
    Write,
}

/// DMA transfer handle, returned by submit().
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaTransferId(u32);

/// DMA-safe buffer handle.
#[derive(Debug, Clone, Copy)]
pub struct DmaHandle {
    pub(crate) pool_offset: u32,
    pub(crate) size: u32,
    pub(crate) owner_task: u32,
}

impl DmaHandle {
    /// The buffer's address, for programming into a DMA engine.
    ///
    /// Guaranteed to be inside the linker's `dma_pool` and 4-byte aligned:
    /// the pool is placed in DRAM the engines can reach, and a descriptor or
    /// buffer that is neither is the failure this type exists to prevent.
    /// A misaligned address does not fault — the engine transfers the wrong
    /// bytes.
    pub fn addr(&self) -> u32 {
        pool_start().wrapping_add(self.pool_offset)
    }
    /// Byte offset of this buffer within the DMA pool.
    pub fn pool_offset(&self) -> u32 {
        self.pool_offset
    }

    /// Size of this buffer in bytes, rounded up to the pool's alignment.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Task that allocated this buffer, and the only one permitted to use it.
    pub fn owner_task(&self) -> u32 {
        self.owner_task
    }
}

// DMA pool region (defined in linker script).
extern "C" {
    static _dma_pool_start: u32;
    static _dma_pool_end: u32;
}

/// Error from the DMA broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    /// Requested size does not fit the remaining pool.
    PoolExhausted,
    /// Caller does not own the handle.
    NotOwner,
    /// Engine programming is not implemented yet (Phase 3+).
    NotImplemented,
}

static NEXT_TRANSFER_ID: AtomicU32 = AtomicU32::new(1);
static mut DMA_OFFSET: u32 = 0;

/// Base address of the pool.
fn pool_start() -> u32 {
    core::ptr::addr_of!(_dma_pool_start) as u32
}

#[cfg(not(test))]
fn pool_size() -> u32 {
    // Taking the address of a static is safe; only reading through it is not,
    // and nothing here does.
    let start = core::ptr::addr_of!(_dma_pool_start) as u32;
    let end = core::ptr::addr_of!(_dma_pool_end) as u32;
    end.saturating_sub(start)
}

/// The stub pool's size, stated rather than derived.
///
/// On the target these come from the linker script and the distance between
/// them is the region. The host stubs are two unrelated statics whose relative
/// placement Rust does not define, so subtracting their addresses gives a
/// meaningless number — zero, as it happens, which made every allocation fail
/// with `PoolExhausted`. Matching `arch::host::linker_stubs::DMA_POOL_WORDS`.
#[cfg(test)]
fn pool_size() -> u32 {
    2048 * 4
}

/// Allocate a DMA-safe buffer (bump allocator over the linker `dma_pool`).
///
/// Item 8: both the alignment step (`size + 3`) and the bump step
/// (`start + aligned`) used unchecked `u32` arithmetic. A `size` near
/// `u32::MAX` wraps `(size + 3) & !3` to a small aligned value, and even
/// with a merely large-but-in-range `aligned`, `start + aligned` can wrap
/// past `u32::MAX` back to a small `end` — in both cases the `end >
/// pool_size()` guard is bypassed by the wrapped value looking small, so the
/// caller gets `Ok` with a handle describing memory outside (or the bump
/// pointer moving *backward*, aliasing already-live allocations rather than
/// past) the pool. Checked arithmetic turns both into the pool-exhausted
/// error the size genuinely earned instead of a bogus success.
pub fn alloc(size: u32) -> Result<DmaHandle, DmaError> {
    crate::scheduler::with(|sched| {
        let aligned = checked_align_up4(size).ok_or(DmaError::PoolExhausted)?;
        let start = unsafe { DMA_OFFSET };
        let end = start.checked_add(aligned).ok_or(DmaError::PoolExhausted)?;
        if end > pool_size() {
            return Err(DmaError::PoolExhausted);
        }
        unsafe { DMA_OFFSET = end };
        Ok(DmaHandle {
            pool_offset: start,
            size: aligned,
            owner_task: sched.current(),
        })
    })
}

/// Round `size` up to the next multiple of 4, or `None` if doing so would
/// overflow `u32` (i.e. `size` is within 3 of `u32::MAX`).
fn checked_align_up4(size: u32) -> Option<u32> {
    size.checked_add(3).map(|v| v & !3)
}

/// Submit a DMA transfer. Validates ownership; engine programming is a Phase-3+
/// item, so this currently reports `NotImplemented` rather than faking success.
pub fn submit(
    handle: &DmaHandle,
    _direction: DmaDirection,
    _peripheral_id: u32,
) -> Result<DmaTransferId, DmaError> {
    let current = crate::scheduler::with(|s| s.current());
    if handle.owner_task != current {
        return Err(DmaError::NotOwner);
    }
    // Reserve an id so the API shape is stable, but do not pretend the transfer
    // ran — the engine is not programmed yet (plan W6.3).
    let _id = NEXT_TRANSFER_ID.fetch_add(1, Ordering::SeqCst);
    Err(DmaError::NotImplemented)
}

/// Block until a DMA transfer completes. Not implemented yet (Phase 3+).
pub fn await_transfer(id: DmaTransferId) -> Result<(), DmaError> {
    let _ = id;
    Err(DmaError::NotImplemented)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsupport;

    /// The bump pointer is process-global, so a test that wants a known amount
    /// of pool has to start from a known state.
    fn rewind() {
        unsafe { DMA_OFFSET = 0 };
    }

    #[test]
    fn a_buffer_is_word_aligned() {
        // A misaligned DMA address does not fault -- the engine transfers the
        // wrong bytes, which looks like a driver bug for a long time.
        let _k = testsupport::lock();
        rewind();
        for size in [1u32, 2, 3, 4, 5, 17, 63] {
            let h = alloc(size).expect("pool should have room");
            assert_eq!(h.addr() % 4, 0, "size {size} gave a misaligned buffer");
            assert!(h.size() >= size, "size {size} came back short");
            assert_eq!(h.size() % 4, 0, "size {size} left the pointer misaligned");
        }
    }

    #[test]
    fn a_buffer_lies_inside_the_pool() {
        // The region guarantee. A buffer outside it is memory the DMA engines
        // may not reach at all -- flash-mapped or RTC -- and the transfer
        // silently moves nothing.
        let _k = testsupport::lock();
        rewind();
        let start = pool_start();
        let end = start + pool_size();
        let h = alloc(256).unwrap();
        assert!(h.addr() >= start, "before the pool");
        assert!(h.addr() + h.size() <= end, "runs past the end of the pool");
    }

    #[test]
    fn two_buffers_do_not_overlap() {
        let _k = testsupport::lock();
        rewind();
        let a = alloc(100).unwrap();
        let b = alloc(100).unwrap();
        assert!(
            a.addr() + a.size() <= b.addr(),
            "the second buffer starts inside the first"
        );
    }

    #[test]
    fn a_request_larger_than_the_pool_is_refused() {
        let _k = testsupport::lock();
        rewind();
        assert_eq!(alloc(pool_size() + 1).unwrap_err(), DmaError::PoolExhausted);
    }

    #[test]
    fn a_size_near_the_top_of_u32_does_not_wrap_into_a_small_buffer() {
        // `(size + 3) & !3` wraps for a size within 3 of u32::MAX, and the
        // wrapped value looks small enough to pass the pool-size check --
        // returning Ok for a buffer that does not exist.
        let _k = testsupport::lock();
        rewind();
        for size in [u32::MAX, u32::MAX - 1, u32::MAX - 2, u32::MAX - 3] {
            assert_eq!(
                alloc(size).unwrap_err(),
                DmaError::PoolExhausted,
                "size {size} wrapped"
            );
        }
    }

    #[test]
    fn the_pool_runs_out_rather_than_handing_out_memory_past_it() {
        let _k = testsupport::lock();
        rewind();
        let chunk = 1024;
        let mut last_end = pool_start();
        while let Ok(h) = alloc(chunk) {
            assert!(h.addr() >= last_end, "handed out overlapping memory");
            last_end = h.addr() + h.size();
            assert!(last_end <= pool_start() + pool_size(), "past the end of the pool");
        }
        // And it stays refused rather than wrapping round.
        assert_eq!(alloc(chunk).unwrap_err(), DmaError::PoolExhausted);
    }

    #[test]
    fn a_transfer_is_refused_for_a_handle_the_caller_does_not_own() {
        let _k = testsupport::lock();
        rewind();
        let mut h = alloc(32).unwrap();
        h.owner_task = h.owner_task.wrapping_add(1);
        assert_eq!(
            submit(&h, DmaDirection::Read, 0).unwrap_err(),
            DmaError::NotOwner,
            "ownership is not checked"
        );
    }
}

