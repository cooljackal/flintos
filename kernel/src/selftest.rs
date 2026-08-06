// SPDX-License-Identifier: Apache-2.0

//! On-target self-tests — the checks that only mean anything on real silicon.
//!
//! The host suite (`make test-host`, on every change) exercises the kernel's
//! logic against the stand-ins in [`crate::arch`]. What a stand-in cannot
//! reproduce is the machine: a register window spilled across a trap, a timer
//! ISR preempting a task mid-computation, a critical section that actually
//! masks an interrupt, a counter that advances because silicon counts. Those
//! are the failures that have historically cost this kernel the most time, and
//! none of them is reachable from a host.
//!
//! So they run here, on a board, when a human asks: `make test-target`.
//!
//! Each test runs after interrupts are unmasked and before the idle task, which
//! is the only window where the interesting failures are possible at all.
//!
//! **Output is a contract.** `tools/target-test.sh` parses these exact lines to
//! decide whether the run passed; changing them changes the harness. The format
//! is deliberately dull — one line per test, a terminating summary — because it
//! is read over a serial port that may drop or garble bytes, and a format that
//! needs more than one line to be interpretable cannot survive that:
//!
//! ```text
//! [FLINT] SELFTEST BEGIN
//! [FLINT] TEST tick_advances PASS
//! [FLINT] TEST critical_section_masks_the_tick FAIL tick advanced while masked
//! [FLINT] SELFTEST END pass=4 fail=1
//! ```

use crate::arch::registers;
use crate::arch::Tick;
use crate::debug::fault::raw_print;
use hal::tick::TickSource;

#[path = "selftest_races.rs"]
mod races;

/// Result of one check. The reason travels with the failure because a bare
/// FAIL over a serial line tells whoever reads it nothing they can act on.
pub(crate) type Check = Result<(), &'static str>;

/// Run every on-target self-test and report.
///
/// Does not panic on failure: a panic would halt at the first bad test and hide
/// every one after it, and the harness would see a truncated stream. Reporting
/// all of them and letting the host decide is strictly more informative.
pub fn run() {
    raw_print("[FLINT] SELFTEST BEGIN\r\n");

    let mut pass = 0u32;
    let mut fail = 0u32;

    check("timer_preserves_windowed_context", timer_preserves_windowed_context(), &mut pass, &mut fail);
    check("deep_window_recursion_returns_intact", deep_window_recursion_returns_intact(), &mut pass, &mut fail);
    check("tick_advances", tick_advances(), &mut pass, &mut fail);
    check("tick_never_goes_backwards", tick_never_goes_backwards(), &mut pass, &mut fail);
    check("critical_section_masks_the_tick", critical_section_masks_the_tick(), &mut pass, &mut fail);

    // Task-versus-ISR races. These are the reason an on-target suite exists at
    // all: the host stand-ins mask nothing, so none of the properties below is
    // even falsifiable there.
    check("nested_critical_sections_stay_masked", races::nested_critical_sections_stay_masked(), &mut pass, &mut fail);
    check("interrupt_depth_returns_to_zero", races::interrupt_depth_returns_to_zero(), &mut pass, &mut fail);
    check("ready_mask_agrees_with_task_states", races::ready_mask_agrees_with_task_states(), &mut pass, &mut fail);
    check("pending_switch_is_taken_once", races::pending_switch_is_taken_once(), &mut pass, &mut fail);
    check("mutex_cycle_under_ticks_leaves_no_residue", races::mutex_cycle_under_ticks_leaves_no_residue(), &mut pass, &mut fail);
    check("isr_queue_delivers_exactly_once", races::isr_queue_delivers_exactly_once(), &mut pass, &mut fail);

    raw_print("[FLINT] SELFTEST END pass=");
    print_u32(pass);
    raw_print(" fail=");
    print_u32(fail);
    raw_print("\r\n");
}

fn check(name: &str, result: Check, pass: &mut u32, fail: &mut u32) {
    raw_print("[FLINT] TEST ");
    raw_print(name);
    match result {
        Ok(()) => {
            *pass += 1;
            raw_print(" PASS\r\n");
        }
        Err(reason) => {
            *fail += 1;
            raw_print(" FAIL ");
            raw_print(reason);
            raw_print("\r\n");
        }
    }
}

/// Decimal `u32` without `core::fmt`.
///
/// `raw_print` writes straight to the UART because it has to work when the
/// kernel is too broken to be trusted with anything else — and that is exactly
/// the state a failing self-test reports from.
fn print_u32(mut v: u32) {
    if v == 0 {
        raw_print("0");
        return;
    }
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    // Digits are ASCII by construction.
    raw_print(unsafe { core::str::from_utf8_unchecked(&buf[i..]) });
}

// ── The tests ───────────────────────────────────────────────────────────────

/// The original bring-up regression: a windowed recursion spanning several
/// timer interrupts must return the right answer.
///
/// `fib(25)` builds a deep chain of `call4`/`entry` frames, so the register
/// file overflows and windows spill to the stack repeatedly. If the trap
/// handler mishandles the interrupted task's windows — the longest bug in this
/// kernel's history — the arithmetic comes back wrong rather than crashing,
/// which is why it is checked by value.
fn timer_preserves_windowed_context() -> Check {
    const EXPECTED: u32 = 75_025; // fib(25)
    if fib(25) == EXPECTED {
        Ok(())
    } else {
        Err("windowed recursion returned the wrong value across a tick")
    }
}

fn fib(n: u32) -> u32 {
    match n {
        0 => 0,
        1 => 1,
        n => fib(n - 1) + fib(n - 2),
    }
}

/// Recurse deeper than the physical register file and check the unwind.
///
/// Distinct from the fib test: that one is broad and shallow, this is narrow
/// and deep. The ESP32's file holds 64 registers, so a chain past ~16 nested
/// `call4`s must spill and restore every window. Summing on the way back out
/// means a single misrestored frame changes the total.
fn deep_window_recursion_returns_intact() -> Check {
    const DEPTH: u32 = 128;
    // 1 + 2 + ... + DEPTH
    const EXPECTED: u32 = DEPTH * (DEPTH + 1) / 2;
    if descend(DEPTH) == EXPECTED {
        Ok(())
    } else {
        Err("deep window spill/restore lost or corrupted a frame")
    }
}

fn descend(n: u32) -> u32 {
    if n == 0 {
        0
    } else {
        n + descend(n - 1)
    }
}

/// The tick must actually advance. Nothing on a host can fail this test,
/// because on a host the tick is whatever the stub says it is.
fn tick_advances() -> Check {
    let start = Tick::now();
    spin_cycles(Tick::ticks_per_period().saturating_mul(3));
    if Tick::now() > start {
        Ok(())
    } else {
        Err("tick did not advance across three tick periods")
    }
}

/// Sampled repeatedly, the tick must never go backwards.
///
/// `tick()` re-bases on `CCOUNT` when it has fallen behind, and `CCOUNT` is a
/// wrapping 32-bit counter — an arithmetic slip there shows up as a tick that
/// jumps backwards, which would make every sleep and timeout in the system
/// wrong in a way that is nearly impossible to diagnose after the fact.
fn tick_never_goes_backwards() -> Check {
    let mut last = Tick::now();
    for _ in 0..10_000 {
        let now = Tick::now();
        if now < last {
            return Err("tick went backwards");
        }
        last = now;
    }
    Ok(())
}

/// A critical section must really mask the timer interrupt.
///
/// The host stand-in for `cs_with` masks nothing — it cannot, there is nothing
/// to mask — so on a host this property is unfalsifiable. Here it is a direct
/// observation: spin for several tick periods inside the critical section and
/// the tick must not move, then confirm it moves again once outside. The second
/// half matters: a tick that never advances at all would pass the first check
/// while being far more broken.
fn critical_section_masks_the_tick() -> Check {
    let per = Tick::ticks_per_period();

    let (before, during) = crate::arch::cs_with(|| {
        let before = Tick::now();
        spin_cycles(per.saturating_mul(3));
        (before, Tick::now())
    });

    if during != before {
        return Err("tick advanced while masked");
    }

    let after_start = Tick::now();
    spin_cycles(per.saturating_mul(3));
    if Tick::now() == after_start {
        return Err("tick never advances at all, masked or not");
    }

    Ok(())
}

/// Busy-wait `cycles` CPU cycles using the free-running counter.
///
/// `CCOUNT` wraps at 2^32; `wrapping_sub` makes the comparison correct across
/// that boundary, which a plain subtraction would get wrong roughly once every
/// 18 seconds at 240 MHz.
pub(crate) fn spin_cycles(cycles: u32) {
    let start = unsafe { registers::read_ccount() };
    while unsafe { registers::read_ccount() }.wrapping_sub(start) < cycles {
        core::hint::spin_loop();
    }
}
