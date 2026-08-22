// SPDX-License-Identifier: Apache-2.0

//! TIMG self-tests. Included by [`crate::selftest`].
//!
//! Two clocks checking each other. The scheduler tick is the Xtensa core's
//! CCOMPARE; TIMG is a separate peripheral off the APB clock. Neither can
//! confirm itself — a timer that reports its own elapsed time is agreeing with
//! itself — so each is measured against the other. A prescaler wrong by a
//! factor of anything shows up immediately.
//!
//! These run from boot context, before the scheduler is running tasks, so they
//! busy-wait on the tick rather than sleeping. That rules out anything needing
//! to block, which is why the alarm test polls a flag the ISR sets rather than
//! waiting on a queue.

use portable_atomic::{AtomicU32, Ordering};

use hal::tick::TickSource;

use crate::arch::Tick;

use super::Check;

/// Microseconds per tick, if both clocks are right.
#[cfg(target_os = "none")]
const US_PER_TICK: u64 = 1000;

/// How long to measure for. Long enough that tick quantisation is noise,
/// short enough not to stall the suite.
#[cfg(target_os = "none")]
const MEASURE_TICKS: u64 = 50;

/// The counter must advance, and at the rate it was asked to.
///
/// A free-running timer at 1 MHz should gain 1000 counts per millisecond of
/// tick. Ten percent either way absorbs the tick's own quantisation and the
/// cost of the latch; a wrong prescaler misses by a factor of 2 or more and a
/// dead timer misses by everything.
#[cfg(target_os = "none")]
pub(crate) fn timg_counts_at_the_rate_it_was_given() -> Check {
    use esp32_timg::{Group, Timer, Timg};

    let t = match unsafe { Timg::new(Group::Timg0, Timer::T0, 1_000_000) } {
        Ok(t) => t,
        Err(_) => return Err("could not configure TIMG0 T0 for 1 MHz"),
    };
    if t.divider() != 80 {
        return Err("1 MHz off an 80 MHz APB clock is not a divider of 80");
    }

    unsafe { t.start_free_running() };

    let t0 = unsafe { t.now() };
    let tick0 = Tick::now();
    while Tick::now().saturating_sub(tick0) < MEASURE_TICKS {
        core::hint::spin_loop();
    }
    let elapsed_ticks = Tick::now().saturating_sub(tick0);
    let elapsed_us = unsafe { t.now() }.saturating_sub(t0);
    unsafe { t.stop() };

    if elapsed_us == 0 {
        // The most likely single failure: reading LO/HI without latching
        // through UPDATE first returns the same stale value forever.
        return Err("the counter never advanced");
    }
    let expected = elapsed_ticks * US_PER_TICK;
    let (lo, hi) = (expected * 9 / 10, expected * 11 / 10);
    if elapsed_us < lo || elapsed_us > hi {
        return Err("the counter advanced at the wrong rate");
    }
    Ok(())
}

/// Fired by the alarm, from interrupt context.
#[cfg(target_os = "none")]
static ALARM_HITS: AtomicU32 = AtomicU32::new(0);

/// CPU interrupt the timer is routed onto for the test.
#[cfg(target_os = "none")]
const TIMG_CPU_INT: u8 = 12;

#[cfg(target_os = "none")]
fn alarm_isr() {
    // A free function, not a handle: a top-half cannot take a lock, so it
    // cannot reach shared state. Acknowledging is one register write.
    unsafe { esp32_timg::clear_interrupt(esp32_timg::Group::Timg0, esp32_timg::Timer::T1) };
    ALARM_HITS.fetch_add(1, Ordering::SeqCst);
}

/// A one-shot alarm must fire, once, from the ISR.
///
/// The issue's actual requirement: "its callback runs from the timer ISR".
/// Counting the hits rather than checking a flag is deliberate — a
/// level-triggered alarm whose handler fails to acknowledge fires forever, and
/// a test that only asks "did it fire" passes hardest in exactly that case.
#[cfg(target_os = "none")]
pub(crate) fn a_timg_alarm_fires_once_from_the_isr() -> Check {
    use esp32_timg::{Group, Mode, Timer, Timg};
    use soc_esp32::addr;

    ALARM_HITS.store(0, Ordering::SeqCst);

    let t = match unsafe { Timg::new(Group::Timg0, Timer::T1, 1_000_000) } {
        Ok(t) => t,
        Err(_) => return Err("could not configure TIMG0 T1"),
    };
    if unsafe { crate::interrupt::connect(addr::IRQ_TIMG0_T1, TIMG_CPU_INT, alarm_isr) }.is_err() {
        return Err("could not connect the timer interrupt");
    }

    // 5 ms, well inside the window below.
    if unsafe { t.start_alarm(5_000, Mode::OneShot) }.is_err() {
        return Err("could not arm the alarm");
    }

    let start = Tick::now();
    while ALARM_HITS.load(Ordering::SeqCst) == 0 {
        if Tick::now().saturating_sub(start) > 100 {
            unsafe { t.stop() };
            return Err("the alarm never fired");
        }
        core::hint::spin_loop();
    }

    // Give a runaway handler room to prove itself. A single acknowledged alarm
    // stays at one; an unacknowledged one climbs without limit.
    super::spin_ticks(20);
    let hits = ALARM_HITS.load(Ordering::SeqCst);
    unsafe { t.stop() };

    if hits != 1 {
        return Err("a one-shot alarm fired more than once — it was not acknowledged");
    }
    Ok(())
}

/// Fired by the periodic alarm.
#[cfg(target_os = "none")]
static PERIODIC_HITS: AtomicU32 = AtomicU32::new(0);

/// A different CPU input from the one-shot test: `interrupt::register`
/// refuses a second handler for an input that already has one, on purpose.
#[cfg(target_os = "none")]
const PERIODIC_CPU_INT: u8 = 17;

/// Period, and how long to watch for. 2 ms over 40 ms is 20 alarms — enough
/// that a rate wrong by any meaningful factor lands outside the window, and
/// short enough not to stall the suite.
#[cfg(target_os = "none")]
const PERIODIC_US: u64 = 2_000;
#[cfg(target_os = "none")]
const WATCH_TICKS: u64 = 40;

#[cfg(target_os = "none")]
fn periodic_isr() {
    unsafe { esp32_timg::clear_interrupt(esp32_timg::Group::Timg1, esp32_timg::Timer::T0) };
    // Re-arm, because ALARM_EN clears itself on fire even in periodic mode:
    // auto-reload reloads the counter, not the alarm. Removing this line is
    // the mutation that proves the claim — without it the count stops at one.
    unsafe { esp32_timg::rearm(esp32_timg::Group::Timg1, esp32_timg::Timer::T0) };
    PERIODIC_HITS.fetch_add(1, Ordering::SeqCst);
}

/// A periodic alarm must keep firing, at the rate it was given.
///
/// The one-shot test proves an alarm can fire. This proves it keeps firing,
/// which is a different mechanism: auto-reload puts the counter back and the
/// handler puts the alarm back, and either one missing gives exactly one
/// alarm rather than none. A test that only asked "did it fire again" would
/// pass on a timer running at any rate at all, so the count is checked
/// against the tick.
#[cfg(target_os = "none")]
pub(crate) fn a_periodic_alarm_keeps_firing_at_its_rate() -> Check {
    use esp32_timg::{Group, Mode, Timer, Timg};
    use soc_esp32::addr;

    PERIODIC_HITS.store(0, Ordering::SeqCst);

    let t = match unsafe { Timg::new(Group::Timg1, Timer::T0, 1_000_000) } {
        Ok(t) => t,
        Err(_) => return Err("could not configure TIMG1 T0"),
    };
    if unsafe { crate::interrupt::connect(addr::IRQ_TIMG1_T0, PERIODIC_CPU_INT, periodic_isr) }
        .is_err()
    {
        return Err("could not connect the periodic timer interrupt");
    }

    if unsafe { t.start_alarm(PERIODIC_US, Mode::Periodic) }.is_err() {
        return Err("could not arm the periodic alarm");
    }

    let start = Tick::now();
    while Tick::now().saturating_sub(start) < WATCH_TICKS {
        core::hint::spin_loop();
    }
    let watched = Tick::now().saturating_sub(start);
    let hits = PERIODIC_HITS.load(Ordering::SeqCst);
    unsafe { t.stop() };

    if hits == 0 {
        return Err("the periodic alarm never fired");
    }
    if hits == 1 {
        // The precise failure this test exists for: one alarm and then
        // silence is auto-reload without a re-arm.
        return Err("the periodic alarm fired once and stopped");
    }
    // Expected count from the *other* clock, so the timer is not marking its
    // own homework.
    let expected = watched * 1000 / PERIODIC_US;
    if (hits as u64) < expected * 3 / 4 || (hits as u64) > expected * 5 / 4 {
        return Err("the periodic alarm fired at the wrong rate");
    }
    Ok(())
}

// Host stand-ins: there is no register block to drive.
#[cfg(not(target_os = "none"))]
pub(crate) fn timg_counts_at_the_rate_it_was_given() -> Check {
    Ok(())
}
#[cfg(not(target_os = "none"))]
pub(crate) fn a_timg_alarm_fires_once_from_the_isr() -> Check {
    let _ = &ALARM_HITS_HOST;
    Ok(())
}
#[cfg(not(target_os = "none"))]
pub(crate) fn a_periodic_alarm_keeps_firing_at_its_rate() -> Check {
    Ok(())
}
#[cfg(not(target_os = "none"))]
static ALARM_HITS_HOST: AtomicU32 = AtomicU32::new(0);
