// SPDX-License-Identifier: Apache-2.0

//! A low-power sleep primitive, for the SoCs that have one.
//!
//! Like [`crate::clock`], the kernel drives this through the trait and selects
//! one implementation with the SoC (`board::SelectedSoc`), so the kernel's
//! neutral `power` module names no chip. A SoC with no sleep FSM implements
//! nothing and takes the default: every call reports [`SleepError::Unsupported`].

/// Why a sleep could not be entered or measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepError {
    /// This SoC has no low-power sleep support — the trait default.
    Unsupported,
    /// The clock that measures elapsed time never latched a sample, so the wake
    /// time could not be computed (the low-power domain's clock is stopped).
    NoClock,
    /// The sleep FSM did not report wake or reject within its poll bound.
    Timeout,
    /// The FSM rejected the sleep — a wake condition was already pending — so
    /// the CPU never paused and no time was lost.
    Rejected,
}

/// A SoC's low-power sleep, selected by the kernel together with the SoC.
///
/// Both methods default to "there is no sleep FSM", so a SoC that has none
/// implements the trait with an empty body and the kernel's `power` module
/// still compiles and dispatches — it just reports [`SleepError::Unsupported`].
pub trait LowPower {
    /// Light sleep for `us` microseconds, then return the *true* elapsed
    /// microseconds measured from a clock that survives the sleep (the CPU
    /// clock is gated, so a cycle counter would freeze). Default: unsupported.
    ///
    /// # Safety
    /// Pauses the CPU. Call only where that is safe — the idle task or a task
    /// that means to block — never from an interrupt.
    unsafe fn light_sleep(_us: u64) -> Result<u64, SleepError> {
        Err(SleepError::Unsupported)
    }

    /// Deep sleep for `us` microseconds. Does not return on success — the wake
    /// arrives as a chip reset. Default: unsupported.
    ///
    /// # Safety
    /// As [`light_sleep`](LowPower::light_sleep).
    unsafe fn deep_sleep(_us: u64) -> Result<(), SleepError> {
        Err(SleepError::Unsupported)
    }
}
