// SPDX-License-Identifier: Apache-2.0

//! Watchdogs: what recovers the board when the kernel stops working.
//!
//! Two of them, catching two different failures. Neither catches the other's.
//!
//! | Watchdog | Fed from | Fires when |
//! |---|---|---|
//! | RTC (RWDT) | the timer interrupt | the kernel has stopped — interrupts stuck masked, a fault loop, a trap handler that never returns |
//! | Timer group (MWDT) | the idle task | a task never yields, so idle never runs, while the tick keeps ticking |
//!
//! The second is the one people forget. A task spinning with interrupts
//! *enabled* keeps the tick alive, so a tick-fed watchdog is fed throughout and
//! the board looks healthy to it — while nothing else in the system will ever
//! run again.
//!
//! # Why the timeouts are what they are
//!
//! Both are generous on purpose. A watchdog that fires during a legitimately
//! long operation trains people to disable it, and a disabled watchdog protects
//! nothing. These are set to catch "stopped", not "slow".
//!
//! The RTC one counts on a 150 kHz RC oscillator with a ±10% tolerance that
//! drifts with temperature, so its timeout is in seconds — at 5 s a 10% error
//! is irrelevant. The MWDT counts off APB and is accurate, but is fed from idle,
//! and idle can legitimately not run for a while under load.

use soc_esp32::wdt;

/// How long the kernel may go without servicing a timer interrupt.
///
/// Comfortably longer than any legitimate critical section. If this ever fires
/// during normal operation, something is holding interrupts masked for over a
/// second, which is a bug in its own right.
pub const KERNEL_TIMEOUT_MS: u32 = 5_000;

/// How long the system may go without the idle task running.
///
/// Longer than the kernel timeout, because a busy system legitimately starves
/// idle for a while and a false reset is worse than a late one.
pub const IDLE_TIMEOUT_MS: u32 = 10_000;

/// The timer group whose watchdog watches the idle task.
///
/// Group 1: group 0 is the more likely home for a general-purpose timer driver
/// later, and a watchdog quietly sharing a peripheral with something else is a
/// conflict nobody looks for.
const IDLE_WDT: wdt::Mwdt = wdt::Mwdt::Group1;

/// Off by default.
///
/// A watchdog is not something to enable behind someone's back: a board that
/// resets itself every five seconds, for reasons its author never asked for, is
/// a very confusing first experience. Applications opt in.
static mut ARMED: bool = false;

/// Arm both watchdogs.
///
/// Call once, after the tick is running — the RTC watchdog is fed from the
/// timer interrupt, so arming it before interrupts are unmasked starts a clock
/// nothing is feeding.
///
/// # Safety
/// Commits the board to resetting itself if the kernel stops. That is the
/// point, but it is a real behaviour change: on a board being single-stepped in
/// a debugger, a halted CPU is indistinguishable from a hung one.
pub unsafe fn arm() {
    wdt::rwdt_arm(KERNEL_TIMEOUT_MS);
    wdt::mwdt_arm(IDLE_WDT, IDLE_TIMEOUT_MS);
    ARMED = true;
}

/// Disarm both. Intended for debugging sessions.
///
/// # Safety
/// After this nothing recovers a hung system short of a power cycle.
pub unsafe fn disarm() {
    ARMED = false;
    wdt::rwdt_disable();
    wdt::mwdt_disable(IDLE_WDT);
}

/// Whether [`arm`] has been called.
pub fn is_armed() -> bool {
    unsafe { core::ptr::addr_of!(ARMED).read_volatile() }
}

/// Feed the kernel watchdog. Called from the timer interrupt.
///
/// Cheap by design — three register writes on a path that runs every
/// millisecond.
#[inline]
pub fn feed_from_tick() {
    if is_armed() {
        unsafe { wdt::rwdt_feed() };
    }
}

/// Feed the idle watchdog. Called from the idle task.
///
/// Deliberately *not* called from the tick. Feeding this one from anywhere that
/// runs regardless of scheduling would defeat its entire purpose: it exists to
/// notice that idle stopped running.
#[inline]
pub fn feed_from_idle() {
    if is_armed() {
        unsafe { wdt::mwdt_feed(IDLE_WDT) };
    }
}
