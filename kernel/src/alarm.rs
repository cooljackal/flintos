// SPDX-License-Identifier: Apache-2.0

//! A one-shot alarm at an absolute microsecond deadline.
//!
//! [`crate::clock`] answers "what time is it". This answers "wake me at". They
//! are different questions and want different hardware: a free-running counter
//! for the first, a programmable compare and an interrupt for the second.
//!
//! # Who wants it
//!
//! The radio blobs. Espressif's Wi-Fi driver arms `ETSTimer`s in
//! *microseconds* — channel dwell, frame timing, retry windows — and gets them
//! back through five entries in `wifi_osi_funcs_t`. The first implementation
//! of those served them from a task that woke every scheduler tick and polled,
//! which made a 100 µs timer fire up to ten times late. All three references
//! use an alarm instead: esp-idf's `esp_timer` programs the LAC timer's
//! compare register and dispatches from its interrupt, NuttX reaches the same
//! code, and Arduino ships esp-idf unchanged.
//!
//! # Which timer, and why the kernel owns it
//!
//! **TG0's LAC counter.** It is a fifth counter inside timer group 0, separate
//! from the four general-purpose timers — which matters, because all four are
//! spoken for: TIMG1/T1 is [`crate::clock`], and TIMG0/T0, TIMG0/T1 and
//! TIMG1/T0 drive on-target self-tests. esp-idf picked the same counter for
//! the same reason.
//!
//! It lives here rather than in the crate that wanted it because of the layer
//! rules, exactly as [`crate::clock`] does: `radio/*` may name `kernel`,
//! `hal`, `soc/*` and `lib/*` but **not `drivers/physical/*`**, so the radio
//! adapter cannot reach TIMG at all.
//!
//! # The callback runs in trap context
//!
//! [`set_handler`] takes a `fn()` and calls it from the interrupt. It must not
//! block and must not take a lock a task could be holding — the usual top-half
//! contract. The radio's use of it is to signal a service task, which is what
//! esp-idf's `timer_alarm_isr` does too: it clears the interrupt and notifies.
//!
//! The handler is registered as **IRAM-safe**, so it keeps running while a
//! flash operation has the cache off. That is a promise about the handler, not
//! something this module can check, and it is why the handler is a `fn()`
//! rather than a closure: whatever is behind it has to be in IRAM.

#[cfg(target_os = "none")]
use esp32_timg::{lact::Lact, Group};

/// The CPU interrupt input the alarm is routed to.
///
/// Level 1, so the trap handler serves it. 19, 20 and 21 are level 2 and would
/// never reach `_flint_trap`; this is the sort of mistake that presents as an
/// alarm that simply never fires.
#[cfg(target_os = "none")]
const ALARM_CPU_INT: u8 = 9;

/// The APB clock, in MHz. Fixed on this chip in every configuration FlintOS
/// builds — there is no dynamic frequency scaling (#38). Derived from the one
/// authority (`soc_esp32::APB_HZ`) rather than restated, so the two cannot
/// drift; a future arch takes this from its own SoC.
#[cfg(target_os = "none")]
const APB_MHZ: u32 = soc_esp32::APB_HZ / 1_000_000;

/// The timer, once `init` has run.
///
/// A `static mut` on the same argument [`crate::clock`] makes: written exactly
/// once during boot on the first core, before anything can bring up a second,
/// and read-only after. A lock would be actively wrong — this is read from
/// trap context, and a reader spinning on a lock held by the task it
/// interrupted deadlocks that core.
#[cfg(target_os = "none")]
static mut ALARM: Option<Lact> = None;

/// What to call when the alarm fires. `None` until [`set_handler`].
static HANDLER: crate::smp::Spinlock<Option<fn()>> = crate::smp::Spinlock::new(None);

/// Claim TG0's LAC timer, route its interrupt, and start counting.
///
/// Failing is not fatal and is reported rather than panicked: the alarm is
/// only needed by the radio, and a board that will not boot because a
/// peripheral it is not using did not come up is worse than one that says so.
///
/// # Safety
/// Takes exclusive ownership of TIMG0's LAC registers and CPU interrupt
/// [`ALARM_CPU_INT`]. Call once, from boot.
#[cfg(target_os = "none")]
pub unsafe fn init() -> bool {
    let Some(lact) = (unsafe { Lact::new(Group::Timg0, APB_MHZ) }) else {
        return false;
    };
    lact.enable_interrupt();
    unsafe { ALARM = Some(lact) };

    // IRAM-safe: `on_alarm` and everything it calls is in IRAM, so the alarm
    // survives a flash operation. A radio timer that stopped for the tens of
    // milliseconds of a sector erase would drop the link.
    let routed = unsafe {
        crate::interrupt::connect_iram_safe_at(
            esp32_timg::lact::TG0_LACT_INTR_SOURCE,
            ALARM_CPU_INT,
            on_alarm,
        )
    };
    if routed.is_err() {
        unsafe { ALARM = None };
        return false;
    }
    true
}

/// The trap-context half.
#[cfg(target_os = "none")]
#[inline(never)]
#[link_section = ".iram1.alarm"]
fn on_alarm() {
    // Acknowledge first. The LAC raises a *level* interrupt, so a handler that
    // returns without clearing it is re-entered immediately and forever.
    if let Some(t) = unsafe { (*core::ptr::addr_of!(ALARM)).as_ref() } {
        t.clear_interrupt();
        // One-shot: the deadline has passed, and leaving the compare armed
        // would re-fire the moment the counter is reloaded.
        t.clear_alarm();
    }
    // `try_with`, not `with`: a top-half that spins on a lock held by the task
    // it interrupted deadlocks that core. Losing one alarm to a concurrent
    // `set_handler` is the better failure, and `set_handler` holds this for
    // one word write.
    if let Some(Some(f)) = HANDLER.try_with(|h| *h) {
        f();
    }
    wake_waiter();
}

/// The address [`wait`] blocks on and the alarm wakes.
///
/// A zero-sized key, not a queue. `kernel::queue`'s waiter lists are keyed by
/// address and do not care what is at it, which is the same trick
/// `dynobj::Semaphore` uses — and reusing that path means the ISR-side wake is
/// one the on-target suite already exercises rather than a new one.
static WAIT_TOKEN: u8 = 0;

#[inline]
fn wait_key() -> usize {
    core::ptr::addr_of!(WAIT_TOKEN) as usize
}

#[cfg(target_os = "none")]
#[link_section = ".iram1.alarm"]
#[inline(never)]
fn wake_waiter() {
    crate::queue::wake_one_receiver(wait_key());
}

/// Block until the alarm fires or `timeout_ms` passes. `true` if woken.
///
/// The timeout is a backstop, not the mechanism: an alarm that is missed makes
/// the caller late rather than never, and on a host — where there is no LAC
/// timer — it is the only thing that returns at all.
///
/// One waiter. A second task calling this would be woken by the same alarm and
/// there is nothing here to say which; the intended caller is a single service
/// task.
pub fn wait(timeout_ms: u32) -> bool {
    crate::queue::block_recv(wait_key(), timeout_ms)
}

/// Install what the alarm calls. Returns the previous handler.
///
/// The handler runs in trap context. See the module docs.
pub fn set_handler(f: Option<fn()>) -> Option<fn()> {
    HANDLER.with(|h| core::mem::replace(h, f))
}

/// Fire the alarm `after_us` microseconds from now.
///
/// Replaces any alarm already set — there is one compare register, so this is
/// a single deadline rather than a queue. The caller keeps the queue and arms
/// the earliest, which is what `radio_esp32::ets_timer` does.
///
/// A delay of zero fires as soon as the hardware can, not never.
pub fn set_after_us(after_us: u64) {
    #[cfg(target_os = "none")]
    if let Some(t) = unsafe { (*core::ptr::addr_of!(ALARM)).as_ref() } {
        let now = t.now_ticks();
        t.set_alarm(now + after_us * esp32_timg::lact::TICKS_PER_US);
    }
    #[cfg(not(target_os = "none"))]
    let _ = after_us;
}

/// Cancel a pending alarm. The counter keeps running.
pub fn cancel() {
    #[cfg(target_os = "none")]
    if let Some(t) = unsafe { (*core::ptr::addr_of!(ALARM)).as_ref() } {
        t.clear_alarm();
    }
}

/// Whether `init` succeeded.
pub fn is_available() -> bool {
    #[cfg(target_os = "none")]
    {
        unsafe { (*core::ptr::addr_of!(ALARM)).is_some() }
    }
    #[cfg(not(target_os = "none"))]
    {
        false
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handler_replaces_rather_than_stacks() {
        fn a() {}
        fn b() {}
        assert!(set_handler(Some(a)).is_none());
        let previous = set_handler(Some(b));
        assert!(previous.is_some(), "the first handler should come back");
        assert!(set_handler(None).is_some());
        assert!(set_handler(None).is_none());
    }

    #[test]
    fn arming_without_hardware_does_nothing_rather_than_panicking() {
        // The host has no LAC timer. Every entry point has to be callable
        // anyway, because the code that arms alarms is shared and its tests
        // run here.
        assert!(!is_available());
        set_after_us(0);
        set_after_us(1_000_000);
        cancel();
    }
}
