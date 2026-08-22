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
