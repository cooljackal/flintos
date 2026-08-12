// SPDX-License-Identifier: Apache-2.0

//! `ETSTimer` — the blob's software timers, and `esp_timer_get_time`.
//!
//! Six table entries: `_timer_setfn`, `_timer_arm`, `_timer_arm_us`,
//! `_timer_disarm`, `_timer_done` and `_esp_timer_get_time`.
//!
//! # The blob owns the memory, we own the meaning
//!
//! An `ETSTimer` is a struct the blob allocates and hands back as a `void *`.
//! Its layout is esp-idf's, and writing into it would mean matching a struct
//! definition that is not in anything the archives ship. So the pointer is
//! used only as an **identity** — a key into a table here — and never
//! dereferenced. That costs a linear scan over at most [`MAX_TIMERS`] entries
//! and buys not having to guess a layout.
//!
//! # Callbacks run on a task, not on the tick
//!
//! The obvious implementation hangs the scan off `kernel::timer`, whose
//! callbacks run in **trap context**. esp-idf's do not: an `ETSTimer`
//! callback runs on the timer *task*, and the blob's callbacks are written for
//! that — they take semaphores, post to queues, and call back into the blob.
//! `kernel::queue` refuses to block in interrupt context, correctly, so those
//! callbacks would start failing in ways that look like the radio misbehaving.
//!
//! So [`start`] spawns a task that sleeps and scans. That also puts a floor
//! under the resolution: the scan runs once per scheduler tick, so a timer
//! armed for less than a tick fires at the next one. `_timer_arm_us` is
//! therefore honest about milliseconds and approximate below that, which is
//! documented on it rather than hidden.
//!
//! # The poll is the wrong shape, and this is where that gets fixed
//!
//! "Approximate below a millisecond" was written here as a limitation. It is
//! a defect, and all three references agree on what the right shape is:
//!
//! - **esp-idf v4.4** routes `ets_timer_arm*` to `esp_timer_start_once` /
//!   `_periodic`, backed by the **TG0 LAC timer** — a 64-bit up-counter with a
//!   programmable alarm and a level interrupt. The ISR does
//!   `vTaskNotifyGiveFromISR`; the callback runs on `timer_task` at
//!   `ESP_TASK_PRIO_MAX - 3`.
//! - **NuttX** does the same, explicitly with
//!   `.dispatch_method = ESP_TIMER_TASK`.
//! - **Arduino** is esp-idf with a different build system, so it inherits it.
//!   Not independent evidence.
//!
//! An alarm, not a poll. `_timer_arm_us(t, 100, false)` here fires at the next
//! millisecond — up to ten times late, and jittered by whatever else is
//! runnable — and the Wi-Fi MAC arms microsecond timers. A state machine given
//! them ten times late is being lied to.
//!
//! That is also the leading explanation for the timing-sensitive hang in
//! `esp_wifi_start` (`doc/plan-radio.md`), because it predicts both of that
//! bug's properties: it needs this task to exist, and it moves when a few
//! microseconds of unrelated work shift the phase of the poll.
//!
//! **The port costs no general-purpose timer.** The LAC timer is a separate
//! counter inside a timer group (`TIMG_LACTCONFIG_REG`), not one of the four
//! GP timers — which is why esp-idf chose it, and it matters here because
//! FlintOS has all four spoken for: TIMG1/T1 is `kernel::clock`, and TIMG0/T0,
//! TIMG0/T1 and TIMG1/T0 drive on-target self-tests.

use core::ffi::c_void;

use kernel::smp::Spinlock;

/// Timers the blob may have at once.
///
/// esp-idf's Wi-Fi driver uses a handful; 16 is slack over what has been
/// observed rather than a measured bound, and running out is reported rather
/// than silently dropping a timer.
pub const MAX_TIMERS: usize = 16;

/// How often the scan task wakes. One scheduler tick — the finest it can
/// usefully be, since `sleep_ms` resolves to ticks anyway.
const SCAN_PERIOD_MS: u32 = 1;

/// One armed or idle timer.
///
/// The callback pointer and its argument are held as `usize` for the same
/// reason as in [`crate::interrupts`]: raw pointers are not `Send`, and this
/// table crosses cores.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Slot {
    /// The blob's `ETSTimer *`, as identity. 0 means the slot is free.
    handle: usize,
    callback: usize,
    arg: usize,
    /// Absolute microseconds to fire at. 0 means disarmed.
    fire_at_us: u64,
    /// Repeat interval in microseconds, or 0 for one-shot.
    period_us: u64,
}

impl Slot {
    const FREE: Self = Slot { handle: 0, callback: 0, arg: 0, fire_at_us: 0, period_us: 0 };
}

static TIMERS: Spinlock<[Slot; MAX_TIMERS]> = Spinlock::new([Slot::FREE; MAX_TIMERS]);

/// Find the slot for `handle`, or claim a free one.
fn slot_for(table: &mut [Slot; MAX_TIMERS], handle: usize) -> Option<usize> {
    if let Some(i) = table.iter().position(|s| s.handle == handle) {
        return Some(i);
    }
    let i = table.iter().position(|s| s.handle == 0)?;
    table[i] = Slot { handle, ..Slot::FREE };
    Some(i)
}

/// Everything due at `now`, taken out of the table and re-armed if periodic.
///
/// Split from the task loop so the decision -- which timers fire, and what
/// the table looks like afterwards -- is testable without a scheduler. The
/// firing itself cannot be: it is a call into the blob.
///
/// Returns how many entries of `out` were filled.
fn collect_due(table: &mut [Slot; MAX_TIMERS], now_us: u64, out: &mut [(usize, usize); MAX_TIMERS]) -> usize {
    let mut n = 0;
    for s in table.iter_mut() {
        if s.handle == 0 || s.fire_at_us == 0 || s.fire_at_us > now_us {
            continue;
        }
        out[n] = (s.callback, s.arg);
        n += 1;
        if s.period_us > 0 {
            // From the deadline, not from now, so a periodic timer does not
            // drift by however late the scan was. Catch up if it fell more
            // than a whole period behind rather than firing in a burst.
            let next = s.fire_at_us.saturating_add(s.period_us);
            s.fire_at_us = if next <= now_us {
                now_us.saturating_add(s.period_us)
            } else {
                next
            };
        } else {
            s.fire_at_us = 0;
        }
    }
    n
}

/// The scan task. Wakes each tick, fires what is due.
fn scan_task() {
    loop {
        api::task::sleep_ms(SCAN_PERIOD_MS);
        let now = kernel::clock::now_us();
        let mut due = [(0usize, 0usize); MAX_TIMERS];
        // The lock is dropped before any callback runs. A blob callback can
        // arm or delete a timer -- including its own -- and doing that from
        // inside this lock would be re-entry, which `Spinlock` panics on.
        let n = TIMERS.with(|t| collect_due(t, now, &mut due));
        for &(cb, arg) in &due[..n] {
            if cb != 0 {
                let f: unsafe extern "C" fn(*mut c_void) = unsafe { core::mem::transmute(cb) };
                unsafe { f(arg as *mut c_void) };
            }
        }
    }
}

/// `ESP_TASK_TIMER_PRIO` — esp-idf's own priority for this task.
///
/// `configMAX_PRIORITIES - 3`, which is 22 of 25. Put through the same
/// inversion the adapter applies to the blob's own tasks, so the timer service
/// lands one step below `wifiT` (23) and above everything else — the ordering
/// esp-idf runs and the blob was tested against.
///
/// It was `Normal(1)`, which was a number with nothing behind it: every task
/// the blob creates maps into the Critical band, so the service that fires the
/// blob's timers could not preempt a single one of them. Taking the number
/// from the reference instead of inventing one is right regardless of what it
/// fixes.
///
/// **It does not fix the hang**, and that is worth recording so the next
/// attempt does not start here. There is a timing-sensitive hang in
/// `esp_wifi_start` on the stored-calibration path that appears only when this
/// task exists — commenting out [`start`] removes it, at the cost of the scan.
/// It survives this priority, and it survives 24 (above `wifiT`), which was
/// tried on the theory that `wifiT` spins rather than blocks. See
/// `doc/plan-radio.md`.
const ESP_TASK_TIMER_PRIO: u32 = 22;

/// Where that lands in FlintOS's numbering.
const TIMER_PRIORITY: hal::types::Priority =
    crate::adapter::priority_from_freertos(ESP_TASK_TIMER_PRIO);

/// `CONFIG_ESP_TIMER_TASK_STACK_SIZE` is 3584. Rounded up, because what runs
/// on this stack is blob callbacks of unknown frame depth — the same reason
/// `radioprobe` asks for 16 KiB — and 512 bytes is cheap insurance against a
/// silent overflow on a kernel with no MPU.
const TIMER_STACK: usize = 4096;

/// Whether [`start`] has already spawned the service task.
static STARTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Start the timer service. Idempotent.
///
/// Nothing arms a timer until the blob does, so this is cheap until then: a
/// task that wakes once a tick and walks 16 slots.
///
/// # Nothing called this, and the radio could not scan
///
/// The five timer entries went into the OSI table, the blob armed timers
/// through them, `collect_due` was written and host-tested — and no task ever
/// ran it, because `start` had no caller anywhere in the tree. A scan hops
/// thirteen channels on a software timer, so it advanced only when the driver
/// gave up on each channel: **8.7 seconds to time out, then a `SCAN_DONE` with
/// zero results.**
///
/// It is called from [`crate::wifi::init`] now, which is the only place that
/// can guarantee "before the blob is initialised", rather than being left to
/// each application to remember. That is the fix for the class as well as the
/// instance: a service the adapter depends on should not be an application's
/// responsibility to start.
pub fn start() {
    use core::sync::atomic::Ordering;
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    api::task::spawn("radio-timer", scan_task, TIMER_PRIORITY, TIMER_STACK);
}

/// `_esp_timer_get_time()` — microseconds since boot.
///
/// `i64` because that is esp-idf's signature. The underlying counter is 64-bit
/// at 1 MHz, so the sign bit is 292,000 years away.
///
/// # Safety
/// Reads a timer. Called by the blob.
#[no_mangle]
pub unsafe extern "C" fn esp_timer_get_time() -> i64 {
    kernel::clock::now_us() as i64
}

/// `_timer_setfn(timer, f, arg)` — attach a callback to a timer.
///
/// Creates the entry if this handle has not been seen. Does not arm it;
/// esp-idf's `ets_timer_setfn` does not either.
///
/// # Safety
/// `timer` is used as identity and never dereferenced. Called by the blob.
#[no_mangle]
pub unsafe extern "C" fn timer_setfn(timer: *mut c_void, f: *mut c_void, arg: *mut c_void) {
    let handle = timer as usize;
    if handle == 0 {
        api::log_error!("radio: _timer_setfn on a null timer");
        return;
    }
    let ok = TIMERS.with(|t| match slot_for(t, handle) {
        Some(i) => {
            t[i].callback = f as usize;
            t[i].arg = arg as usize;
            true
        }
        None => false,
    });
    if !ok {
        api::log_error!("radio: no free timer slot; MAX_TIMERS is {}", MAX_TIMERS);
    }
}

/// Shared by [`timer_arm`] and [`timer_arm_us`].
fn arm(handle: usize, delay_us: u64, repeat: bool, what: &str) {
    if handle == 0 {
        api::log_error!("radio: {} on a null timer", what);
        return;
    }
    let now = kernel::clock::now_us();
    let ok = TIMERS.with(|t| match slot_for(t, handle) {
        Some(i) => {
            t[i].fire_at_us = now.saturating_add(delay_us).max(1);
            t[i].period_us = if repeat { delay_us } else { 0 };
            true
        }
        None => false,
    });
    if !ok {
        api::log_error!("radio: no free timer slot; MAX_TIMERS is {}", MAX_TIMERS);
    }
}

/// `_timer_arm(timer, ms, repeat)`.
///
/// # Safety
/// `timer` is identity only. Called by the blob.
#[no_mangle]
pub unsafe extern "C" fn timer_arm(timer: *mut c_void, ms: u32, repeat: bool) {
    arm(timer as usize, (ms as u64).saturating_mul(1_000), repeat, "_timer_arm");
}

/// `_timer_arm_us(timer, us, repeat)`.
///
/// **Resolution is one scheduler tick, not one microsecond.** The scan task
/// wakes on the tick, so anything armed for less than that fires at the next
/// one. The blob uses this for timeouts in the tens of milliseconds, where
/// the difference does not arise; a genuine sub-millisecond timer would need
/// a hardware alarm per timer rather than a scan.
///
/// # Safety
/// `timer` is identity only. Called by the blob.
#[no_mangle]
pub unsafe extern "C" fn timer_arm_us(timer: *mut c_void, us: u32, repeat: bool) {
    arm(timer as usize, us as u64, repeat, "_timer_arm_us");
}

/// `_timer_disarm(timer)` — stop it firing, keep the callback.
///
/// # Safety
/// `timer` is identity only. Called by the blob.
#[no_mangle]
pub unsafe extern "C" fn timer_disarm(timer: *mut c_void) {
    let handle = timer as usize;
    TIMERS.with(|t| {
        if let Some(s) = t.iter_mut().find(|s| s.handle == handle) {
            s.fire_at_us = 0;
            s.period_us = 0;
        }
    });
}

/// `_timer_done(timer)` — release the entry.
///
/// The blob frees its own `ETSTimer` after this, so the handle must not stay
/// in the table: the same address can come back as a different timer, and a
/// stale entry would fire someone else's callback.
///
/// # Safety
/// `timer` is identity only. Called by the blob.
#[no_mangle]
pub unsafe extern "C" fn timer_done(timer: *mut c_void) {
    let handle = timer as usize;
    TIMERS.with(|t| {
        if let Some(s) = t.iter_mut().find(|s| s.handle == handle) {
            *s = Slot::FREE;
        }
    });
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> [Slot; MAX_TIMERS] {
        [Slot::FREE; MAX_TIMERS]
    }

    #[test]
    fn a_one_shot_fires_once_and_disarms() {
        let mut t = empty();
        let i = slot_for(&mut t, 0x1000).unwrap();
        t[i] = Slot { handle: 0x1000, callback: 7, arg: 8, fire_at_us: 500, period_us: 0 };

        let mut out = [(0, 0); MAX_TIMERS];
        assert_eq!(collect_due(&mut t, 499, &mut out), 0, "not due yet");
        assert_eq!(collect_due(&mut t, 500, &mut out), 1);
        assert_eq!(out[0], (7, 8));
        assert_eq!(collect_due(&mut t, 5_000, &mut out), 0, "a one-shot must not repeat");
    }

    #[test]
    fn a_periodic_timer_keeps_its_phase() {
        // Re-arming from `now` rather than from the deadline makes a periodic
        // timer drift by however late each scan was -- which accumulates.
        let mut t = empty();
        t[0] = Slot { handle: 1, callback: 1, arg: 0, fire_at_us: 1_000, period_us: 1_000 };

        let mut out = [(0, 0); MAX_TIMERS];
        assert_eq!(collect_due(&mut t, 1_050, &mut out), 1, "fired 50us late");
        assert_eq!(t[0].fire_at_us, 2_000, "next deadline is on the original phase");
    }

    #[test]
    fn a_periodic_timer_that_fell_behind_catches_up_rather_than_bursting() {
        // If the scan misses many periods -- a long flash write, say -- firing
        // once per missed period would flood the blob with callbacks it can no
        // longer act on.
        let mut t = empty();
        t[0] = Slot { handle: 1, callback: 1, arg: 0, fire_at_us: 1_000, period_us: 1_000 };

        let mut out = [(0, 0); MAX_TIMERS];
        assert_eq!(collect_due(&mut t, 50_000, &mut out), 1, "one callback, not fifty");
        assert_eq!(t[0].fire_at_us, 51_000, "re-armed from now, not from a stale phase");
    }

    #[test]
    fn a_disarmed_timer_does_not_fire() {
        let mut t = empty();
        t[0] = Slot { handle: 1, callback: 1, arg: 0, fire_at_us: 0, period_us: 0 };
        let mut out = [(0, 0); MAX_TIMERS];
        assert_eq!(collect_due(&mut t, u64::MAX, &mut out), 0);
    }

    #[test]
    fn one_handle_gets_one_slot_however_often_it_is_armed() {
        // `_timer_setfn` then `_timer_arm` is the normal sequence, and each
        // goes through `slot_for`. A second slot per timer would leak the
        // table in sixteen calls.
        let mut t = empty();
        let a = slot_for(&mut t, 0xAAAA).unwrap();
        let b = slot_for(&mut t, 0xAAAA).unwrap();
        assert_eq!(a, b);
        assert_eq!(t.iter().filter(|s| s.handle != 0).count(), 1);
    }

    #[test]
    fn a_full_table_is_reported_rather_than_overwriting_someone() {
        let mut t = empty();
        for h in 1..=MAX_TIMERS {
            assert!(slot_for(&mut t, h * 0x10).is_some());
        }
        assert_eq!(slot_for(&mut t, 0xDEAD), None, "the 17th must be refused");
    }

    #[test]
    fn a_freed_handle_leaves_no_entry_behind() {
        // The blob frees the ETSTimer after `_timer_done`, and the allocator
        // can hand the same address back. A stale entry would then fire the
        // old callback for a new timer.
        let mut t = empty();
        t[0] = Slot { handle: 0x2000, callback: 9, arg: 9, fire_at_us: 10, period_us: 0 };
        t[0] = Slot::FREE;
        let mut out = [(0, 0); MAX_TIMERS];
        assert_eq!(collect_due(&mut t, u64::MAX, &mut out), 0);
        assert_eq!(slot_for(&mut t, 0x2000), Some(0), "the slot is reusable");
    }

    #[test]
    fn several_timers_due_at_once_all_fire() {
        let mut t = empty();
        for (i, s) in t.iter_mut().enumerate().take(3) {
            *s = Slot { handle: i + 1, callback: i + 100, arg: 0, fire_at_us: 10, period_us: 0 };
        }
        let mut out = [(0, 0); MAX_TIMERS];
        assert_eq!(collect_due(&mut t, 10, &mut out), 3);
    }
}
