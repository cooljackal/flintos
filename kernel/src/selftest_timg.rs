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

use core::sync::atomic::{AtomicU32, Ordering};

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
    use soc_esp32::{addr, intr_map};

    ALARM_HITS.store(0, Ordering::SeqCst);

    let t = match unsafe { Timg::new(Group::Timg0, Timer::T1, 1_000_000) } {
        Ok(t) => t,
        Err(_) => return Err("could not configure TIMG0 T1"),
    };
    if unsafe { intr_map::route(addr::IRQ_TIMG0_T1, TIMG_CPU_INT) }.is_err() {
        return Err("could not route the timer interrupt");
    }
    if !crate::interrupt::register(TIMG_CPU_INT, alarm_isr) {
        return Err("that CPU interrupt already has a handler");
    }
    unsafe { crate::arch::registers::enable_interrupt(TIMG_CPU_INT as u32) };

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
    let settle = Tick::now();
    while Tick::now().saturating_sub(settle) < 20 {
        core::hint::spin_loop();
    }
    let hits = ALARM_HITS.load(Ordering::SeqCst);
    unsafe { t.stop() };

    if hits != 1 {
        return Err("a one-shot alarm fired more than once — it was not acknowledged");
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
static ALARM_HITS_HOST: AtomicU32 = AtomicU32::new(0);
