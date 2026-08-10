// SPDX-License-Identifier: Apache-2.0

//! Xtensa `CriticalSection` implementation (plan W2.1).
//!
//! Masks interrupts up to `CRITICAL_SECTION_PRIORITY` by raising `PS.INTLEVEL`
//! via `rsil`, and restores the previous `PS` when the token is dropped. This
//! does **not** mask debug/NMI level interrupts, preserving real-time response
//! for the highest-priority events.

use hal::critical_section::{CriticalSection, CriticalSectionToken};

/// Interrupt level masked inside a critical section. Level-1 application and
/// driver interrupts (incl. the timer tick) are masked; higher levels are not.
pub const CRITICAL_SECTION_PRIORITY: u32 = 1;

/// The Xtensa critical section.
pub struct XtensaCriticalSection;

/// Token holding the saved `PS` to restore on drop.
#[must_use = "dropping the token immediately ends the critical section"]
pub struct XtensaCsToken {
    saved_ps: u32,
}

impl CriticalSection for XtensaCriticalSection {
    type Token = XtensaCsToken;

    #[inline(always)]
    fn enter() -> Self::Token {
        // The token is the raw pair with a `Drop` bolted on, rather than a
        // second copy of the same `rsil`. There is exactly one instruction
        // sequence for entering and one for leaving, in `enter_raw` and
        // `exit_raw` below.
        XtensaCsToken {
            saved_ps: unsafe { enter_raw() },
        }
    }
}

impl CriticalSectionToken for XtensaCsToken {
    fn release(self) {
        // Drop does the work.
        drop(self);
    }
}

impl Drop for XtensaCsToken {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe { exit_raw(self.saved_ps) }
    }
}

/// Enter a critical section without a token, returning the saved `PS`.
///
/// **For foreign code that cannot take a closure.** Everything written in Rust
/// must use [`with`], whose token restores `PS` on drop and therefore cannot
/// be left unbalanced — that is the whole reason this module offers a closure
/// and not a pair of functions.
///
/// Espressif's radio blobs call `phy_enter_critical()` and
/// `phy_exit_critical()` as two separate C functions with the region between
/// them, and no closure spans that. So the pair exists, marked `unsafe`, and
/// the safety contract is the thing the token used to guarantee.
///
/// # Safety
/// Every call must be matched by exactly one [`exit_raw`] on the same core,
/// passing back the value returned here. Failing to call it leaves interrupts
/// masked for good, which presents as a kernel that has stopped rather than as
/// anything pointing here.
#[inline(always)]
pub unsafe fn enter_raw() -> u32 {
    let saved_ps: u32;
    unsafe {
        core::arch::asm!("rsil {0}, {1}", out(reg) saved_ps, const CRITICAL_SECTION_PRIORITY);
    }
    saved_ps
}

/// Leave a critical section entered with [`enter_raw`].
///
/// # Safety
/// `saved_ps` must be the value returned by the matching [`enter_raw`].
/// Passing anything else writes an arbitrary value to `PS`, which is a good
/// deal worse than leaving interrupts masked.
#[inline(always)]
pub unsafe fn exit_raw(saved_ps: u32) {
    unsafe {
        core::arch::asm!("wsr.ps {0}", "rsync", in(reg) saved_ps);
    }
}

/// Convenience: run `f` with interrupts masked, returning its result.
/// `inline(always)`: code running on the second core lives in IRAM, and a
/// real call from there into this (in flash) would hang -- that core has no
/// instruction cache.
#[inline(always)]
pub fn with<R>(f: impl FnOnce() -> R) -> R {
    let _token = XtensaCriticalSection::enter();
    f()
}
