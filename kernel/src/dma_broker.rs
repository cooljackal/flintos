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

fn pool_size() -> u32 {
    // Taking the address of a static is safe; only reading through it is not,
    // and nothing here does.
    let start = core::ptr::addr_of!(_dma_pool_start) as u32;
    let end = core::ptr::addr_of!(_dma_pool_end) as u32;
    end.saturating_sub(start)
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
            owner_task: sched.current,
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
    let current = crate::scheduler::with(|s| s.current);
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
