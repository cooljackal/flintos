// SPDX-License-Identifier: Apache-2.0

//! DMA Broker (Phase 3).
//!
//! Manages a pool of DMA-safe SRAM and provides safe submission/await
//! for physical driver tasks.  Drivers never touch DMA engine registers
//! — the broker validates and programs them.

use portable_atomic::{AtomicU32, Ordering};

// DMA pool region (defined in linker script).
extern "C" {
    static _dma_pool_start: u32;
    static _dma_pool_end: u32;
}

/// Broker types that a driver — and `api` — must name without naming the
/// kernel. They live in `hal` (like [`DmaError`]) and are re-exported here so
/// existing `kernel::dma_broker::` paths keep working.
///
/// [`DmaHandle`] carries a resolved address rather than a pool offset: the
/// pool base is a linker symbol only the kernel can see (see [`pool_start`]),
/// so [`alloc`] resolves it once and the handle is portable thereafter.
pub use hal::dma::{DmaError, DmaHandle, DmaTransferId};

static NEXT_TRANSFER_ID: AtomicU32 = AtomicU32::new(1);
/// Bump allocator offset into the DMA pool.
///
/// A `static mut`, and safe for the same reason as `spawn`'s: every access is
/// inside `scheduler::with` (`alloc` opens with it, and the handle it returns
/// records `sched.current()`, so the lock is needed regardless).
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
        // Resolve the pool-relative offset to an absolute address here, while
        // the linker symbol is in reach: the handle is portable afterwards.
        Ok(DmaHandle::new(
            pool_start().wrapping_add(start),
            aligned,
            sched.current(),
        ))
    })
}

/// Bytes still free in the pool.
///
/// The bump allocator never frees, so this only falls. A caller that wants to
/// size a buffer to what the pool can actually spare — the display's DMA
/// chunk, which follows the pool rather than a hardcoded constant — reads this
/// once before allocating.
pub fn available() -> u32 {
    crate::scheduler::with(|_| pool_size().saturating_sub(unsafe { DMA_OFFSET }))
}

/// Round `size` up to the next multiple of 4, or `None` if doing so would
/// overflow `u32` (i.e. `size` is within 3 of `u32::MAX`).
fn checked_align_up4(size: u32) -> Option<u32> {
    size.checked_add(3).map(|v| v & !3)
}

// ── Completion ──────────────────────────────────────────────────────────────
//
// The broker does not program the engine, and the `submit` that used to sit
// here could not have: there is no portable engine to program. The ESP32 has a
// three-channel crossbar for SPI; an S3 has GDMA; an STM32 has numbered
// streams. `soc_esp32::dma` says all this at length, and register programming
// belongs to the driver that owns the peripheral.
//
// What is the kernel's business is the part a driver cannot do from
// `drivers/physical`: blocking a task until an interrupt says the transfer has
// finished. Physical drivers may depend on `hal` and `soc/*` only, so they have
// no queues and no scheduler. They raise the interrupt; this waits on it.

/// Completion ids, published by a driver's top-half and consumed by the task
/// that started the transfer.
///
/// Depth 4 rather than 1: a task that gives up on a timeout leaves its
/// completion to arrive afterwards, and a queue with no room for it would hand
/// that stale one to the next transfer's wait.
static COMPLETIONS: api::queue::Queue<u32, 4> = api::queue::Queue::new();

/// Begin a transfer and get the id its completion will carry.
///
/// Checks the caller owns the buffer. Starting the engine is the driver's job;
/// this only mints the id the two sides agree on.
pub fn begin(handle: &DmaHandle) -> Result<DmaTransferId, DmaError> {
    let current = crate::scheduler::with(|s| s.current());
    if handle.owner_task() != current {
        return Err(DmaError::NotOwner);
    }
    Ok(DmaTransferId::from_raw(
        NEXT_TRANSFER_ID.fetch_add(1, Ordering::SeqCst),
    ))
}

/// Signal that `id` has finished. **Call from a driver's top-half.**
///
/// The queue's ISR path, which wakes a blocked receiver. A full queue drops
/// the message rather than retrying: an ISR that spins waiting for a task to
/// drain something is an ISR that never returns.
pub fn signal_complete(id: DmaTransferId) {
    let _ = COMPLETIONS.send_isr(id.raw());
}

/// Block until `id` completes, or `timeout_ms` passes.
///
/// A real block, not a poll. It was a 1 ms sampling loop for exactly as long
/// as `queue::deadline_for` was taking the scheduler lock recursively and
/// panicking every blocking receive; with that fixed the task can be
/// descheduled properly and woken by the interrupt.
///
/// Completions for *other* transfers are discarded rather than put back. A
/// stale id belongs to a transfer whose waiter has already given up, and
/// returning it to the queue would spin this loop against something that never
/// drains.
pub fn await_transfer(id: DmaTransferId, timeout_ms: u32) -> Result<(), DmaError> {
    let deadline = api::timer::now_ms().saturating_add(timeout_ms as u64);
    loop {
        let left = deadline.saturating_sub(api::timer::now_ms());
        if left == 0 {
            return Err(DmaError::Timeout);
        }
        match api::queue::recv(&COMPLETIONS, left as u32) {
            Ok(got) if got == id.raw() => return Ok(()),
            Ok(_) => continue,
            Err(_) => return Err(DmaError::Timeout),
        }
    }
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
            assert!(
                last_end <= pool_start() + pool_size(),
                "past the end of the pool"
            );
        }
        // And it stays refused rather than wrapping round.
        assert_eq!(alloc(chunk).unwrap_err(), DmaError::PoolExhausted);
    }

    #[test]
    fn a_transfer_is_refused_for_a_handle_the_caller_does_not_own() {
        // The buffer's owner is the only task allowed to start a transfer over
        // it. Without this a task could hand the engine a buffer another task
        // is still writing.
        let _k = testsupport::lock();
        rewind();
        let h = alloc(32).unwrap();
        // Same buffer, a different owner: the field is private now, so rebuild
        // the handle rather than poking it.
        let not_mine = DmaHandle::new(h.addr(), h.size(), h.owner_task().wrapping_add(1));
        assert_eq!(
            begin(&not_mine).unwrap_err(),
            DmaError::NotOwner,
            "ownership is not checked"
        );
    }

    #[test]
    fn each_transfer_gets_a_distinct_id() {
        // Two transfers sharing an id would let one's completion release the
        // other's waiter, which is a data race with the engine still running.
        let _k = testsupport::lock();
        rewind();
        let h = alloc(32).unwrap();
        let a = begin(&h).unwrap();
        let b = begin(&h).unwrap();
        assert_ne!(a, b, "two transfers were given the same id");
    }
}
