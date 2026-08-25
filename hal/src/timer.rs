// SPDX-License-Identifier: Apache-2.0

//! A one-shot compare timer: "interrupt me `after_us` microseconds from now".
//!
//! Distinct from [`crate::clock`] (a free-running counter, "what time is it")
//! and [`crate::tick`] (the periodic scheduling interrupt). The kernel's `alarm`
//! module drives one of these for the radio's microsecond timers, selecting one
//! implementation with the SoC so its neutral half names no chip.
//!
//! # The IRAM contract
//!
//! [`clear_alarm`](CompareTimer::clear_alarm) and
//! [`clear_interrupt`](CompareTimer::clear_interrupt) are called from the
//! alarm's interrupt handler, which is registered IRAM-safe so it keeps firing
//! while a flash operation has the instruction cache off. An implementation's
//! bodies for those two must therefore be in RAM (or inline into the caller),
//! exactly like the driver methods underneath them; a flash-resident forwarder
//! would wedge the moment the cache went away.

/// A one-shot compare timer, selected by the kernel together with the SoC.
pub trait CompareTimer: Sized {
    /// Claim and start the timer, or `None` if the SoC has none free.
    ///
    /// # Safety
    /// Takes exclusive ownership of the timer's registers; nothing else may
    /// drive it.
    unsafe fn claim() -> Option<Self>;

    /// The peripheral interrupt source to route to a CPU interrupt input.
    fn interrupt_source(&self) -> u8;

    /// Unmask the timer's interrupt. Called once, at init.
    fn enable_interrupt(&self);

    /// Fire an interrupt `after_us` microseconds from now. Replaces any pending
    /// alarm — there is one compare register, so this is a single deadline.
    fn set_after_us(&self, after_us: u64);

    /// Disarm a pending alarm. The counter keeps running.
    fn clear_alarm(&self);

    /// Acknowledge a fired interrupt. The source is level-triggered, so a
    /// handler that returns without clearing it re-enters forever.
    fn clear_interrupt(&self);
}
