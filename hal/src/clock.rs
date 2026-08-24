// SPDX-License-Identifier: Apache-2.0

//! A free-running monotonic microsecond clock.
//!
//! Distinct from [`crate::tick::TickSource`]: the tick is the periodic
//! scheduling interrupt, counted in milliseconds; this is a free-running
//! counter read for absolute time and for measuring intervals shorter than a
//! tick. Not every SoC has a spare hardware counter to spend on one, so both
//! methods default to "there isn't one" — a SoC without a monotonic counter is
//! answered from the scheduler tick instead, a thousand times coarser but still
//! monotonic and correctly ordered.

/// A monotonic microsecond clock, for the SoCs that have a spare counter.
///
/// The kernel selects one implementation and reads it through this trait, so a
/// caller that wants absolute time names no chip. A SoC with no such counter
/// implements nothing and takes the defaults; the kernel then falls back to the
/// tick.
pub trait MonotonicClock {
    /// Claim and start the counter. Call once from boot, before a second core
    /// exists to race the single write it makes. Returns `false` when the SoC
    /// has no such counter or it could not be claimed — the default.
    ///
    /// # Safety
    /// Takes exclusive ownership of a hardware timer; nothing else may drive it.
    unsafe fn init() -> bool {
        false
    }

    /// Microseconds since [`init`], or `None` when this SoC has no monotonic
    /// counter — in which case the caller falls back to the scaled tick.
    ///
    /// Monotonic: it only counts up, including across a task switch on either
    /// core, because it is one hardware counter rather than kernel bookkeeping.
    ///
    /// [`init`]: MonotonicClock::init
    fn now_us() -> Option<u64> {
        None
    }
}
