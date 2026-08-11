// SPDX-License-Identifier: Apache-2.0

//! Does a flash erase still let an interrupt through? Included by
//! [`crate::selftest`].
//!
//! A sector erase runs with the instruction cache disabled and takes tens of
//! milliseconds. Masking every interrupt for that long is safe and is a
//! real-time defect: long enough to drop a Wi-Fi link, and long enough to
//! matter without a radio anywhere near it. So the kernel masks *selectively*
//! — handlers registered through `interrupt::register_iram_safe` stay live,
//! everything else is masked in `INTENABLE` — and the trap path those handlers
//! arrive through is in IRAM.
//!
//! None of that is checkable from a host, and none of it is checkable by
//! reading the code either: `#[link_section]` says where a function body goes
//! and nothing about a copy the optimiser folded into a caller, which is a trap
//! this project has fallen into repeatedly. The only thing that settles it is
//! an interrupt firing on real silicon while a real erase is in progress.
//!
//! # The control is the tick
//!
//! "An interrupt fired during the erase" is a weak claim on its own — it would
//! also pass if the masking had simply been forgotten and everything ran. What
//! makes this a test of *selectivity* is measuring, across the same erase, one
//! interrupt that promised IRAM and one that did not.
//!
//! The scheduler tick is the second one, for free. It is the busiest interrupt
//! in the system and it is emphatically not IRAM-safe, so during the erase it
//! must be masked and `Tick::now()` must stand still while the alarm count
//! climbs. Both numbers come out of the same window, so a build that masked
//! everything and a build that masked nothing each fail a different assertion.
//!
//! # Confirmed by mutation
//!
//! Swapping `connect_iram_safe` for `connect` — the one-word change that
//! says this handler made no promises — turns the pass into
//! "the IRAM-safe alarm stopped being serviced during the erase". So the
//! test is measuring the mask and not the weather.

use core::sync::atomic::{AtomicU32, Ordering};

use hal::tick::TickSource;

use crate::arch::Tick;

use super::Check;

/// Alarms serviced. Written from the ISR with the cache off.
#[cfg(target_os = "none")]
static ALARM_HITS: AtomicU32 = AtomicU32::new(0);

/// TIMG0 T0: the one timer no other self-test attaches an *interrupt* to. T0
/// of group 1 is the periodic-alarm test's, T1 of group 0 is the one-shot
/// test's, and T1 of group 1 is `kernel::clock`. The rate test also drives
/// TIMG0 T0, but free-running and with no alarm, and this runs before it.
///
/// CPU input 13, and the number is not free choice. The trap handler serves
/// level 1 only, and the ESP32 assigns each CPU input a fixed level: 19, 20
/// and 21 are level 2, so routing here would have produced an interrupt that
/// fires into `_flint_unhandled` and an alarm that never appears to arrive.
/// 12 and 17 are the other two timer tests'; `register` refuses to share.
#[cfg(target_os = "none")]
const CPU_INT: u8 = 13;

/// One alarm per millisecond. An erase is tens of milliseconds, so a working
/// window produces tens of hits and a broken one produces nought or one.
#[cfg(target_os = "none")]
const ALARM_US: u64 = 1_000;

/// The scratch sector.
///
/// `phy_init` from the standard espressif partition table — 4 KiB at
/// `0x0000f000`, which esp-idf uses for a PHY calibration blob and **FlintOS
/// does not touch**: its calibration goes in `nvs` through `kvstore`. Erasing
/// here destroys nothing, which is the property that matters for a test that
/// runs on every `make test-target`.
///
/// Not `nvs`: that holds a stored RF calibration, and silently erasing it
/// would cost the next boot a few hundred milliseconds of recalibration and
/// make `apps/radioprobe` report a first boot forever.
#[cfg(target_os = "none")]
const SCRATCH_BASE: u32 = 0x0000_F000;
#[cfg(target_os = "none")]
const SCRATCH_LEN: u32 = 0x0000_1000;

/// The top-half. **Everything it touches is in IRAM**, which is the promise
/// `connect_iram_safe` takes on trust.
///
/// `clear_interrupt` and `rearm` are both placed there by `esp32-timg` for
/// exactly this caller. The counter is an atomic in DRAM. There is no lock,
/// which is the other half of the promise: the second core is stalled in
/// hardware for the duration of the erase, and waiting on something a stalled
/// core holds does not end.
#[cfg(target_os = "none")]
#[inline(never)]
#[link_section = ".iram1.selftest"]
fn alarm_isr() {
    unsafe { esp32_timg::clear_interrupt(esp32_timg::Group::Timg0, esp32_timg::Timer::T0) };
    // Periodic reloads the counter, not the alarm; see the periodic test.
    unsafe { esp32_timg::rearm(esp32_timg::Group::Timg0, esp32_timg::Timer::T0) };
    ALARM_HITS.fetch_add(1, Ordering::SeqCst);
}

/// An IRAM-safe interrupt keeps being serviced across a flash erase, and a
/// non-IRAM-safe one does not.
#[cfg(target_os = "none")]
pub(crate) fn an_erase_does_not_stop_an_iram_safe_interrupt() -> Check {
    use esp32_timg::{Group, Mode, Timer, Timg};
    use soc_esp32::addr;

    ALARM_HITS.store(0, Ordering::SeqCst);

    let t = match unsafe { Timg::new(Group::Timg0, Timer::T0, 1_000_000) } {
        Ok(t) => t,
        Err(_) => return Err("could not configure TIMG0 T0"),
    };
    if unsafe { crate::interrupt::connect_iram_safe(addr::IRQ_TIMG0_T0, CPU_INT, alarm_isr) }
        .is_err()
    {
        return Err("could not connect the alarm as IRAM-safe");
    }
    if unsafe { t.start_alarm(ALARM_US, Mode::Periodic) }.is_err() {
        return Err("could not arm the alarm");
    }

    // Prove the alarm works at all before asking whether it survives anything.
    // Without this, a timer that never started would sail through the real
    // check below as "no hits during the erase", and be reported as a masking
    // failure it had nothing to do with.
    super::spin_ticks(5);
    if ALARM_HITS.load(Ordering::SeqCst) == 0 {
        unsafe { t.stop() };
        return Err("the alarm never fired, before any flash was touched");
    }

    let region = unsafe { esp32_flash::FlashRegion::new(SCRATCH_BASE, SCRATCH_LEN) };

    // The measurement. `clock::now_us` is TIMG1 T1 free-running: a counter, not
    // an interrupt, so it keeps time through a window in which interrupts are
    // masked — which is exactly what the tick cannot do, and why the tick is
    // the control rather than the clock.
    let hits_before = ALARM_HITS.load(Ordering::SeqCst);
    let tick_before = Tick::now();
    let us_before = crate::clock::now_us();

    let erased = unsafe { region.erase_sector(0) };

    let us_after = crate::clock::now_us();
    let tick_after = Tick::now();
    let hits_after = ALARM_HITS.load(Ordering::SeqCst);
    unsafe { t.stop() };

    if erased.is_err() {
        return Err("the scratch sector would not erase");
    }

    let elapsed_us = us_after.saturating_sub(us_before);
    let hits = hits_after.saturating_sub(hits_before) as u64;
    let ticks = tick_after.saturating_sub(tick_before);

    // A sector erase is tens of milliseconds. If this one was not, the test
    // measured nothing and must say so rather than pass.
    if elapsed_us < 5_000 {
        return Err("the erase returned too fast to have erased anything");
    }

    // The claim. Allow for the alarm being re-armed by the handler rather than
    // free-running, and for the handler itself costing time: half the ideal
    // rate is still unambiguous against the zero a masked interrupt gives.
    let expected = elapsed_us / ALARM_US;
    if hits < expected / 2 {
        return Err("the IRAM-safe alarm stopped being serviced during the erase");
    }

    // The control. The tick is not IRAM-safe, so it must have been masked for
    // most of the window. One or two get through at the edges — the mask goes
    // up after the operation is entered and comes down before it returns — so
    // this is a comparison, not an equality.
    if ticks * 4 > hits {
        return Err("the tick ran through the erase too: the mask is not selective");
    }
    Ok(())
}
