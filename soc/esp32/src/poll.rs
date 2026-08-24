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
//! # Prefer the microsecond bound
//!
//! [`until_us`] bounds the wait by wall-clock time and is what a driver should
//! reach for. [`until`]'s `max_spins` is a raw loop count, so its real duration
//! depends on the CPU clock — the same number means one thing at 80 MHz and a
//! third of it at 240 MHz, and nothing at all on another core. It remains for
//! the loops that run before the clock is installed, or with the instruction
//! cache off (a flash erase), where the microsecond clock cannot be read.
//!
//! The clock the microsecond bound reads is [installed by the kernel](set_clock)
//! at boot: its timer is a Layer-1 driver a Layer-0 SoC crate may not depend on,
//! so the kernel hands the reader down rather than this crate reaching up for
//! it — the same shape as `esp32_flash::set_interrupt_hooks`.

use core::sync::atomic::{AtomicUsize, Ordering};

/// A poll gave up before its condition held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeout;

/// A generous default iteration bound for [`until`]. A 12-bit ADC conversion or
/// a short frame is microseconds; this absorbs interrupts and still fails dead
/// hardware.
pub const DEFAULT_SPINS: u32 = 100_000;

/// A generous default timeout for [`until_us`], in microseconds. Absorbs an
/// interrupt storm and still fails dead hardware quickly, at any CPU frequency.
pub const DEFAULT_TIMEOUT_US: u64 = 50_000;

/// The monotonic microsecond clock, installed by [`set_clock`]. Zero — meaning
/// "not installed" — until the kernel calls it during boot. A function pointer
/// stored as a `usize`: written once before any driver polls, read lock-free
/// from any context (including trap context), like `startup`'s console handle.
static NOW_US: AtomicUsize = AtomicUsize::new(0);

/// Install the monotonic microsecond clock the [`until_us`] bound reads.
///
/// Called once from boot, before any driver polls. Passing a null-equivalent is
/// impossible: a `fn` pointer is never zero, so the "not installed" sentinel can
/// never collide with a real reader.
pub fn set_clock(now_us: fn() -> u64) {
    NOW_US.store(now_us as usize, Ordering::Release);
}

/// Read the installed clock, or `None` before one is installed.
#[inline]
fn now_us() -> Option<u64> {
    match NOW_US.load(Ordering::Acquire) {
        0 => None,
        // SAFETY: only ever written by `set_clock` from a real `fn` pointer of
        // this exact type; zero is the reserved "unset" value.
        p => Some(unsafe { core::mem::transmute::<usize, fn() -> u64>(p) }()),
    }
}

/// Spin until `ready()` returns true, or `max_spins` iterations pass.
///
/// Returns [`Timeout`] on expiry. `ready` is polled at least once. Prefer
/// [`until_us`] unless the wait runs before the clock is installed or with the
/// cache off.
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

/// Spin until `ready()` returns true, or `timeout_us` microseconds pass — a
/// bound that means the same wall-clock time at any CPU frequency, unlike
/// [`until`].
///
/// `ready` is polled at least once. Before the kernel installs a clock (early
/// boot) it falls back to [`until`] with [`DEFAULT_SPINS`], so a poll that races
/// boot still terminates.
#[inline]
pub fn until_us(timeout_us: u64, mut ready: impl FnMut() -> bool) -> Result<(), Timeout> {
    if ready() {
        return Ok(());
    }
    let Some(start) = now_us() else {
        // No clock yet — fall back to the iteration bound.
        return until(ready, DEFAULT_SPINS);
    };
    let deadline = start.saturating_add(timeout_us);
    while !ready() {
        // A stopped clock reads a constant; treat a non-advancing reading as
        // expired rather than spinning forever on it.
        if now_us().unwrap_or(u64::MAX) >= deadline {
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
    use core::sync::atomic::AtomicU64;

    /// A test clock that advances 10 µs on every read, so a never-satisfied
    /// condition reaches any finite deadline. Process-global like the real one;
    /// each `until_us` test captures its own start and deadline, so a shared,
    /// monotonically advancing counter does not couple them.
    static TEST_NOW: AtomicU64 = AtomicU64::new(0);
    fn test_clock() -> u64 {
        TEST_NOW.fetch_add(10, Ordering::Relaxed)
    }

    #[test]
    fn until_us_times_out_when_the_condition_never_holds() {
        set_clock(test_clock);
        assert_eq!(until_us(100, || false), Err(Timeout));
    }

    #[test]
    fn until_us_returns_ok_before_the_deadline_when_the_condition_holds() {
        set_clock(test_clock);
        let n = Cell::new(0u32);
        // Ready on the third poll; the 10 000 µs bound is far beyond the few
        // 10 µs steps that takes, so this must succeed, not time out.
        let r = until_us(10_000, || {
            n.set(n.get() + 1);
            n.get() >= 3
        });
        assert_eq!(r, Ok(()));
        assert!(n.get() >= 3);
    }

    #[test]
    fn until_us_is_ok_immediately_when_already_satisfied() {
        // Checked before the clock is even read, so this holds with or without
        // an installed clock.
        assert_eq!(until_us(0, || true), Ok(()));
    }

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
