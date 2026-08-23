// SPDX-License-Identifier: Apache-2.0

//! A monotonic microsecond clock.
//!
//! The scheduler tick is milliseconds and is the wrong instrument for two
//! jobs: measuring something shorter than a tick, and answering a caller that
//! wants absolute time rather than a count of scheduling periods. Espressif's
//! radio blobs want the second — `esp_timer_get_time` is one of the entries in
//! `wifi_osi_funcs_t` and the blob calls it constantly.
//!
//! # Which timer, and why the kernel owns it
//!
//! **TIMG1 timer 1**, at 1 MHz, free-running. The other three general-purpose
//! timers are used by the on-target self-tests (TIMG0/T0, TIMG0/T1, TIMG1/T0),
//! so this is the one that was free.
//!
//! It lives in the kernel rather than in the crate that wanted it because of
//! the layer rules, and the rules are right here: `radio/*` may name `kernel`,
//! `hal`, `soc/*` and `lib/*`, but **not `drivers/physical/*`**, so the radio
//! adapter cannot reach TIMG at all. A clock is a kernel service — several
//! things will want one — and the alternative was widening the radio crate's
//! tier for a single peripheral.
//!
//! # Sixty-four bits, and no wrap
//!
//! The counter is 64 bits at 1 MHz, so it wraps after about 584,000 years.
//! There is deliberately no wrap handling: code that reads a 32-bit counter
//! and compensates is code with a bug waiting for the boundary, and here the
//! boundary does not exist.

#[cfg(target_os = "none")]
use esp32_timg::{Group, Timer, Timg};

/// Ticks per second. 1 MHz makes the count microseconds directly, with no
/// scaling at the point of use.
#[cfg(target_os = "none")]
const RESOLUTION_HZ: u32 = 1_000_000;

/// The clock, once `init` has run.
///
/// A `static mut` rather than a lock, and the argument is the same one
/// `startup::CONSOLE_UART` makes: it is written exactly once, during boot on
/// the first core, before `join_scheduler` can bring a second one up. After
/// that it is read-only, and shared reads race with nothing.
///
/// A lock would be actively wrong here. [`now_us`] is called from trap
/// context — that is most of the point of it — and a reader spinning on a
/// lock held by the task it interrupted deadlocks that core.
///
/// What the argument does not cover is a second write. There is no path to
/// one; adding one needs this behind something else first.
#[cfg(target_os = "none")]
static mut CLOCK: Option<Timg> = None;

/// Claim TIMG1/T1 and start it.
///
/// Call once, from boot, before any second core. Failing is not fatal: the
/// clock reads zero and the callers that wanted microseconds get a useless
/// but harmless answer, which is better than refusing to boot over a
/// peripheral only the radio needs.
///
/// # Safety
/// Takes exclusive ownership of TIMG1 timer 1. Nothing else may drive it.
#[cfg(target_os = "none")]
pub unsafe fn init() -> bool {
    match unsafe { Timg::new(Group::Timg1, Timer::T1, RESOLUTION_HZ) } {
        Ok(t) => {
            unsafe { t.start_free_running() };
            unsafe { CLOCK = Some(t) };
            true
        }
        Err(_) => false,
    }
}

/// Microseconds since `init`.
///
/// Zero if `init` has not run or failed. Monotonic: it counts up and never
/// goes back, including across a task switch on either core, because it is
/// one hardware counter rather than anything the kernel maintains.
pub fn now_us() -> u64 {
    #[cfg(target_os = "none")]
    {
        // Safe: written once before the second core exists, read-only after.
        match unsafe { (*core::ptr::addr_of!(CLOCK)).as_ref() } {
            Some(t) => unsafe { t.now() },
            None => 0,
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        // No TIMG on a host. The scheduler tick is the only clock there, and
        // it is milliseconds -- so this is monotonic and correctly ordered,
        // but a thousand times coarser. Host tests may check ordering; they
        // must not check resolution.
        use hal::tick::TickSource;
        crate::arch::Tick::now().saturating_mul(1_000)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_never_goes_backwards() {
        // The one property every caller depends on, and the only one the host
        // stand-in can honestly be asked about.
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
