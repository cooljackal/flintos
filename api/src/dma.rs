// SPDX-License-Identifier: Apache-2.0

//! DMA buffers and transfers, brokered by the kernel.
//!
//! A physical driver never touches a DMA engine's registers directly: it asks
//! the broker for a buffer in reachable memory ([`alloc`]), tells it a transfer
//! is starting ([`begin`]), programs its own engine against the buffer's
//! address, and blocks until the transfer's interrupt reports completion
//! ([`await_transfer`]). The broker owns the pool, the ownership check, and the
//! block-until-done; the driver owns the engine.

pub use hal::{DmaError, DmaHandle, DmaTransferId};

/// Allocate a DMA-safe buffer of at least `size` bytes.
///
/// The returned [`DmaHandle`] is 4-byte aligned and inside memory the SoC's
/// DMA engines can reach. Fails with [`DmaError::PoolExhausted`] when the pool
/// has no room left.
pub fn alloc(size: u32) -> Result<DmaHandle, DmaError> {
    extern "Rust" {
        fn _flint_sys_dma_alloc(size: u32) -> Result<DmaHandle, DmaError>;
    }
    unsafe { _flint_sys_dma_alloc(size) }
}

/// Bytes still free in the DMA pool.
///
/// The pool is a bump allocator with no free, so this only falls over a run.
/// A driver that sizes a buffer to what the pool can spare — rather than to a
/// constant that has to be right for every board's pool at once — reads this
/// before [`alloc`]. The display's DMA chunk follows the pool this way.
pub fn available() -> u32 {
    extern "Rust" {
        fn _flint_sys_dma_available() -> u32;
    }
    unsafe { _flint_sys_dma_available() }
}

/// Begin a transfer over `handle` and get the id its completion will carry.
///
/// Only the task that allocated the buffer may start a transfer over it;
/// another task gets [`DmaError::NotOwner`]. Starting the engine is still the
/// driver's job — this only mints the id the top-half and the waiter agree on.
pub fn begin(handle: &DmaHandle) -> Result<DmaTransferId, DmaError> {
    extern "Rust" {
        fn _flint_sys_dma_begin(handle: &DmaHandle) -> Result<DmaTransferId, DmaError>;
    }
    unsafe { _flint_sys_dma_begin(handle) }
}

/// Block until `id` completes, or `timeout_ms` passes.
///
/// A real block: the task is descheduled and woken by the transfer's
/// interrupt, not spun in a poll. Returns [`DmaError::Timeout`] if the
/// completion does not arrive in time.
pub fn await_transfer(id: DmaTransferId, timeout_ms: u32) -> Result<(), DmaError> {
    extern "Rust" {
        fn _flint_sys_dma_await(id: DmaTransferId, timeout_ms: u32) -> Result<(), DmaError>;
    }
    unsafe { _flint_sys_dma_await(id, timeout_ms) }
}
