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

    fn enter() -> Self::Token {
        let saved_ps: u32;
        unsafe {
            // rsil writes the new level and returns the previous PS.
            core::arch::asm!("rsil {0}, {1}", out(reg) saved_ps, const CRITICAL_SECTION_PRIORITY);
        }
        XtensaCsToken { saved_ps }
    }
}

impl CriticalSectionToken for XtensaCsToken {
    fn release(self) {
        // Drop does the work.
        drop(self);
    }
}

impl Drop for XtensaCsToken {
    fn drop(&mut self) {
        unsafe {
            core::arch::asm!("wsr.ps {0}", "rsync", in(reg) self.saved_ps);
        }
    }
}

/// Convenience: run `f` with interrupts masked, returning its result.
#[inline]
pub fn with<R>(f: impl FnOnce() -> R) -> R {
    let _token = XtensaCriticalSection::enter();
    f()
}
