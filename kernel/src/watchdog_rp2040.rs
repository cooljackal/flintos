// SPDX-License-Identifier: Apache-2.0

//! RP2040 kernel watchdog policy.
//!
//! RP2040 has one watchdog, so it is fed by the timer interrupt and detects a
//! stopped kernel. It cannot independently detect an unyielding task while the
//! tick remains live; [`feed_from_idle`] is therefore intentionally a no-op.

use portable_atomic::{AtomicBool, Ordering};

/// How long the kernel may go without servicing a timer interrupt.
pub const KERNEL_TIMEOUT_MS: u32 = 5_000;

static ARMED: AtomicBool = AtomicBool::new(false);

/// Arm the chip watchdog after the timer interrupt is running.
///
/// # Safety
/// The board will reset if timer interrupts stop for five seconds.
pub unsafe fn arm() {
    unsafe { soc_rp2040::watchdog::arm(KERNEL_TIMEOUT_MS, true) };
    ARMED.store(true, Ordering::Release);
}

/// Stop watchdog recovery for a debugging session.
///
/// # Safety
/// A stopped kernel will no longer recover without external reset.
pub unsafe fn disarm() {
    ARMED.store(false, Ordering::Release);
    unsafe { soc_rp2040::watchdog::disarm() };
}

pub fn is_armed() -> bool {
    ARMED.load(Ordering::Acquire)
}

#[inline]
pub fn feed_from_tick() {
    if is_armed() {
        unsafe { soc_rp2040::watchdog::feed(KERNEL_TIMEOUT_MS) };
    }
}

/// RP2040 has no second watchdog that can independently watch idle.
#[inline]
pub fn feed_from_idle() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_timeout_fits_the_hardware_counter() {
        assert_eq!(
            soc_rp2040::watchdog::load_for_ms(KERNEL_TIMEOUT_MS),
            10_000_000
        );
    }
}
