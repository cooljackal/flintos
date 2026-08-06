// SPDX-License-Identifier: Apache-2.0

//! Timer services.
//!
//! Provides `now_ms()` for wall-clock time, `once_ms()` for one-shot
//! callbacks, `every_ms()` for periodic callbacks, and `cancel()` to
//! stop a pending timer.
//!
//! # Example
//!
//! ```ignore
//! use api::timer::{once_ms, now_ms};
//!
//! fn callback() { /* ... */ }
//!
//! let start = now_ms();
//! let handle = once_ms(100, callback);
//! ```

/// Return the current tick count (milliseconds since boot).
pub fn now_ms() -> u64 {
    extern "Rust" {
        fn _flint_sys_timer_now_ms() -> u64;
    }
    unsafe { _flint_sys_timer_now_ms() }
}

/// Schedule a one-shot callback after `ms` milliseconds.
pub fn once_ms(ms: u32, callback: fn()) -> TimerHandle {
    extern "Rust" {
        fn _flint_sys_timer_once(ms: u32, cb: fn()) -> u32;
    }
    let id = unsafe { _flint_sys_timer_once(ms, callback) };
    TimerHandle(id)
}

/// Schedule a repeating callback every `ms` milliseconds.
pub fn every_ms(ms: u32, callback: fn()) -> TimerHandle {
    extern "Rust" {
        fn _flint_sys_timer_every(ms: u32, cb: fn()) -> u32;
    }
    let id = unsafe { _flint_sys_timer_every(ms, callback) };
    TimerHandle(id)
}

/// Cancel a timer.
pub fn cancel(handle: TimerHandle) {
    extern "Rust" {
        fn _flint_sys_timer_cancel(id: u32);
    }
    unsafe { _flint_sys_timer_cancel(handle.0) }
}

/// Opaque handle returned by [`once_ms`] / [`every_ms`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerHandle(pub u32);

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_handle_new() {
        let h = TimerHandle(42);
        assert_eq!(h.0, 42);
    }

    #[test]
    fn timer_handle_copy() {
        let a = TimerHandle(7);
        let _b = a;
        let _c = a;
    }

    #[test]
    fn timer_handle_eq() {
        assert_eq!(TimerHandle(1), TimerHandle(1));
        assert_ne!(TimerHandle(1), TimerHandle(2));
    }
}
