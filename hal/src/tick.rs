// SPDX-License-Identifier: Apache-2.0

//! Hardware timer tick source trait.
//!
//! The system tick drives preemptive scheduling, timeouts, and sleep.
//! Each architecture provides its own implementation (e.g. Xtensa CCOUNT
//! + CCOMPARE0 for ESP32).

/// A tick source provides the periodic interrupt that drives the scheduler.
pub trait TickSource {
    /// Initialise the timer to fire at `period_us` microsecond intervals,
    /// given the CPU frequency the timer counts at.
    ///
    /// `cpu_hz` is passed in rather than measured here. Measuring it needs
    /// both a cycle counter (the arch's) and a reference clock (the SoC's),
    /// and no single crate below the kernel may name both -- the Xtensa
    /// implementation used to do it anyway, by carrying its own copy of an
    /// ESP32 peripheral's registers.
    fn init(period_us: u32, cpu_hz: u32);

    /// Called from the tick ISR.  Returns `true` if a context switch is
    /// needed (e.g. the current task's quantum has expired).
    fn tick() -> bool;

    /// Returns the tick count since boot.
    ///
    /// Used by `timer::now_ms()`, `task::sleep_ms()`, and timeout
    /// calculations.
    fn now() -> u64;
}