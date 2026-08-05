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

/// DMA pool region (defined in linker script).
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
    unsafe {
        let start = core::ptr::addr_of!(_dma_pool_start) as u32;
        let end = core::ptr::addr_of!(_dma_pool_end) as u32;
        end.saturating_sub(start)
    }
}

/// Allocate a DMA-safe buffer (bump allocator over the linker `dma_pool`).
pub fn alloc(size: u32) -> Result<DmaHandle, DmaError> {
    crate::scheduler::with(|sched| {
        let aligned = (size + 3) & !3;
        let start = unsafe { DMA_OFFSET };
        let end = start + aligned;
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
