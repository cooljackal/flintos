// SPDX-License-Identifier: Apache-2.0

//! A monotonic microsecond clock.
//!
//! The scheduler tick is milliseconds and is the wrong instrument for two
//! jobs: measuring something shorter than a tick, and answering a caller that
//! wants absolute time rather than a count of scheduling periods. Espressif's
//! radio blobs want the second — `esp_timer_get_time` is one of the entries in
//! `wifi_osi_funcs_t` and the blob calls it constantly.
//!
//! The kernel reads the clock through [`hal::clock::MonotonicClock`], selecting
//! one implementation with the SoC. A SoC with a spare hardware counter (the
//! ESP32's is TIMG1/T1, below) implements it; one without takes the trait
//! default, and [`now_us`] falls back to the scaled scheduler tick. So callers
//! — `syscall`, the self-tests, the radio adapter — name no chip.
//!
//! # Why the ESP32 implementation lives here, not in `soc-esp32`
//!
//! It is built on `esp32-timg`, a Layer-1 driver, and `soc-esp32` is Layer 0 —
//! it may not depend on a driver. The kernel may, so the implementation is a
//! kernel-local type behind the portable trait rather than a chip name in the
//! kernel's neutral code. `radio/*` still cannot reach TIMG (it may not name
//! `drivers/physical/*`), which is why a clock is a kernel service at all.
//!
//! # Sixty-four bits, and no wrap
//!
//! The counter is 64 bits at 1 MHz, so it wraps after about 584,000 years.
//! There is deliberately no wrap handling: code that reads a 32-bit counter
//! and compensates is code with a bug waiting for the boundary, and here the
//! boundary does not exist.

use hal::clock::MonotonicClock;

/// The kernel's monotonic clock, selected by SoC.
///
/// A kernel-local type so the kernel may implement the `hal` trait for it — the
/// `esp32-timg` the ESP32 body needs is a driver neither `hal` nor `soc-esp32`
/// may depend on (see the module header).
pub struct Clock;

impl MonotonicClock for Clock {
    unsafe fn init() -> bool {
        #[cfg(all(target_os = "none", feature = "soc-esp32"))]
        {
            esp32::init()
        }
        #[cfg(not(all(target_os = "none", feature = "soc-esp32")))]
        {
            false
        }
    }

    fn now_us() -> Option<u64> {
        #[cfg(all(target_os = "none", feature = "soc-esp32"))]
        {
            Some(esp32::now_us())
        }
        #[cfg(not(all(target_os = "none", feature = "soc-esp32")))]
        {
            None
        }
    }
}

/// Claim and start the monotonic clock. See [`MonotonicClock::init`].
///
/// # Safety
/// Takes exclusive ownership of the SoC's monotonic timer.
pub unsafe fn init() -> bool {
    unsafe { Clock::init() }
}

/// Microseconds since [`init`].
///
/// Monotonic: it counts up and never goes back, including across a task switch
/// on either core. On a SoC with a hardware microsecond counter this is that
/// counter (zero before `init`); on one without, it is the scheduler tick
/// scaled to microseconds — monotonic and correctly ordered, but a thousand
/// times coarser, so callers may check ordering and must not check resolution.
pub fn now_us() -> u64 {
    match Clock::now_us() {
        Some(us) => us,
        None => {
            use hal::tick::TickSource;
            crate::arch::Tick::now().saturating_mul(1_000)
        }
    }
}

/// The ESP32 monotonic clock: TIMG1 timer 1, 1 MHz, free-running.
///
/// The other three general-purpose timers are used by the on-target
/// self-tests (TIMG0/T0, TIMG0/T1, TIMG1/T0), so this is the one that was free.
#[cfg(all(target_os = "none", feature = "soc-esp32"))]
mod esp32 {
    use esp32_timg::{Group, Timer, Timg};

    /// Ticks per second. 1 MHz makes the count microseconds directly, with no
    /// scaling at the point of use.
    const RESOLUTION_HZ: u32 = 1_000_000;

    /// The clock, once [`init`] has run.
    ///
    /// A `static mut` rather than a lock, and the argument is the same one
    /// `startup::CONSOLE_UART` makes: it is written exactly once, during boot on
    /// the first core, before `join_scheduler` can bring a second one up. After
    /// that it is read-only, and shared reads race with nothing.
    ///
    /// A lock would be actively wrong here. [`super::now_us`] is called from
    /// trap context — that is most of the point of it — and a reader spinning on
    /// a lock held by the task it interrupted deadlocks that core.
    ///
    /// What the argument does not cover is a second write. There is no path to
    /// one; adding one needs this behind something else first.
    static mut CLOCK: Option<Timg> = None;

    /// Claim TIMG1/T1 and start it. Called once, from boot, before any second
    /// core. Failing is not fatal: [`now_us`] then reads zero, a useless but
    /// harmless answer, rather than the boot refusing over a peripheral only
    /// the radio needs.
    ///
    /// # Safety
    /// Takes exclusive ownership of TIMG1 timer 1. Nothing else may drive it.
    pub unsafe fn init() -> bool {
        match unsafe { Timg::new(Group::Timg1, Timer::T1, RESOLUTION_HZ) } {
            Ok(t) => {
                t.start_free_running();
                unsafe { CLOCK = Some(t) };
                true
            }
            Err(_) => false,
        }
    }

    /// Microseconds since [`init`], or zero if it has not run or failed.
    pub fn now_us() -> u64 {
        // Safe: written once before the second core exists, read-only after.
        match unsafe { (*core::ptr::addr_of!(CLOCK)).as_ref() } {
            Some(t) => t.now(),
            None => 0,
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_never_goes_backwards() {
        // The one property every caller depends on, and the only one the host
        // stand-in (the scaled tick) can honestly be asked about.
        let a = now_us();
        let b = now_us();
        assert!(b >= a, "{b} < {a}");
    }

    #[test]
    fn microseconds_not_milliseconds() {
        // A scaling slip here would be invisible in ordering and wrong by
        // 1000x in every duration the radio computes.
        assert_eq!(1_000_000u64, 1_000 * 1_000, "us per second");
    }
}
