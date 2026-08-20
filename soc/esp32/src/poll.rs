// SPDX-License-Identifier: Apache-2.0

//! Busy-wait on a hardware condition, with a bound, spelled once.
//!
//! A dozen drivers wrote the same loop:
//!
//! ```ignore
//! let mut spins = 0;
//! while reg::read(r) & DONE == 0 {
//!     spins += 1;
//!     if spins > SPINS { return Err(..Timeout); }
//!     core::hint::spin_loop();
//! }
//! ```
//!
//! Individually fine; together, twelve places to get the polarity or the bound
//! wrong. [`until`] is that loop, once, returning [`Timeout`] the driver maps
//! into its own error.
//!
//! # The bound is iterations, not time — for now
//!
//! `max_spins` is a raw loop count, so its real duration depends on the CPU
//! clock: the same number means one thing at 80 MHz and a third of it at
//! 240 MHz, and nothing at all on another core. This is a deliberate stopgap.
//! The right bound is a duration, which needs a portable cycle counter — an
//! `Arch::cycle_counter` hook (see `doc/plan-arm32.md`, Phase 2.4). When that
//! lands, this becomes `until_us(deadline_us, …)` and the per-driver counts
//! become microseconds. Until then, keep the count generous.

/// A poll gave up before its condition held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeout;

/// A generous default bound. A 12-bit ADC conversion or a short frame is
/// microseconds; this absorbs interrupts and still fails dead hardware.
pub const DEFAULT_SPINS: u32 = 100_000;

/// Spin until `ready()` returns true, or `max_spins` iterations pass.
///
/// Returns [`Timeout`] on expiry. `ready` is polled at least once.
#[inline]
pub fn until(mut ready: impl FnMut() -> bool, max_spins: u32) -> Result<(), Timeout> {
    let mut spins = 0u32;
    while !ready() {
        spins += 1;
        if spins > max_spins {
            return Err(Timeout);
        }
        core::hint::spin_loop();
    }
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    #[test]
    fn returns_ok_as_soon_as_the_condition_holds() {
        let n = Cell::new(0u32);
        // Ready on the third poll.
        let r = until(
            || {
                n.set(n.get() + 1);
                n.get() >= 3
            },
            100,
        );
        assert_eq!(r, Ok(()));
        assert_eq!(n.get(), 3);
    }

    #[test]
    fn a_condition_that_never_holds_times_out() {
        assert_eq!(until(|| false, 10), Err(Timeout));
    }

    #[test]
    fn a_condition_true_on_the_first_poll_never_spins() {
        // max_spins = 0 must still succeed if ready() is already true, because
        // it is checked before the counter — an off-by-one here would reject a
        // condition that was already satisfied.
        assert_eq!(until(|| true, 0), Ok(()));
    }
}
