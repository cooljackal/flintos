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

#[path = "selftest_dport.rs"]
mod dport;

#[path = "selftest_timg.rs"]
mod timg;

#[path = "selftest_adc.rs"]
mod adc;

#[path = "selftest_dac_adc2.rs"]
mod dac_adc2;

#[path = "selftest_twai.rs"]
mod twai;

#[path = "selftest_heap.rs"]
mod heap;

#[path = "selftest_dynobj.rs"]
mod dynobj;

#[path = "selftest_flash.rs"]
mod flash;

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
    check("call8_windows_survive_preemption", call8_windows_survive_preemption(), &mut pass, &mut fail);
    check("an_erase_does_not_stop_an_iram_safe_interrupt", flash::an_erase_does_not_stop_an_iram_safe_interrupt(), &mut pass, &mut fail);
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

    check("dport_read_agrees_with_a_plain_read", dport::dport_read_agrees_with_a_plain_read(), &mut pass, &mut fail);
    check("dport_read_leaves_the_tick_running", dport::dport_read_leaves_the_tick_running(), &mut pass, &mut fail);
    check("timg_counts_at_the_rate_it_was_given", timg::timg_counts_at_the_rate_it_was_given(), &mut pass, &mut fail);
    check("a_timg_alarm_fires_once_from_the_isr", timg::a_timg_alarm_fires_once_from_the_isr(), &mut pass, &mut fail);

    check("a_periodic_alarm_keeps_firing_at_its_rate", timg::a_periodic_alarm_keeps_firing_at_its_rate(), &mut pass, &mut fail);

    // Needs a pin the *board* holds high; the chip cannot supply one. See
    // `board::active::ADC_EXTERNAL_HIGH_GPIO`.
    match crate::board::active::ADC_EXTERNAL_HIGH_GPIO {
        Some(gpio) => check(
            "adc1_follows_the_pin_it_is_pointed_at",
            adc::adc1_follows_the_pin_it_is_pointed_at(gpio),
            &mut pass,
            &mut fail,
        ),
        None => skip(
            "adc1_follows_the_pin_it_is_pointed_at",
            "this board declares no externally-held-high ADC1 pin",
        ),
    }
    check("every_adc1_channel_converts", adc::every_adc1_channel_converts(), &mut pass, &mut fail);

    // DAC↔ADC2 shared-pad loopback: GPIO 25/26 are both DAC and ADC2 channels,
    // so the DAC's output is read straight back on the same pin. Tests both
    // drivers and the ADC2 radio interlock.
    check("dac_drives_and_adc2_reads_it_back", dac_adc2::dac_drives_and_adc2_reads_it_back(), &mut pass, &mut fail);
    check("adc2_refuses_a_read_while_the_radio_is_up", dac_adc2::adc2_refuses_a_read_while_the_radio_is_up(), &mut pass, &mut fail);

    // TWAI (CAN) self-test loopback: transmit a frame and self-receive it on
    // one pad. Needs a free GPIO the board declares.
    match crate::board::active::LOOPBACK_SCRATCH_GPIO {
        Some(pin) => check(
            "twai_self_reception_round_trips",
            twai::twai_self_reception_round_trips(pin),
            &mut pass,
            &mut fail,
        ),
        None => skip(
            "twai_self_reception_round_trips",
            "this board declares no free loopback GPIO",
        ),
    }

    check("dport_modify_changes_only_its_own_bit", dport::dport_modify_changes_only_its_own_bit(), &mut pass, &mut fail);

    // The radio heap. The allocator is host-tested; what needs the chip is
    // that the reclaimed regions are real, writable, and nobody else's.
    check("reclaimed_memory_is_available", heap::reclaimed_memory_is_available(), &mut pass, &mut fail);
    check("general_memory_holds_a_pattern", heap::general_memory_holds_a_pattern(), &mut pass, &mut fail);
    check("two_allocations_do_not_overlap", heap::two_allocations_do_not_overlap(), &mut pass, &mut fail);
    check("dma_memory_is_where_dma_can_reach", heap::dma_memory_is_where_dma_can_reach(), &mut pass, &mut fail);
    check("every_allocation_is_dma_capable", heap::every_allocation_is_dma_capable(), &mut pass, &mut fail);
    check("the_pool_returns_to_full_after_use", heap::the_pool_returns_to_full_after_use(), &mut pass, &mut fail);

    // Dynamic kernel objects. The allocator logic is host-tested; these need
    // the chip because a task's stack_base is a u32 and the host heap is not.
    check("a_dynamic_task_returns_its_stack", dynobj::a_dynamic_task_returns_its_stack(), &mut pass, &mut fail);
    check("task_churn_does_not_leak", dynobj::task_churn_does_not_leak(), &mut pass, &mut fail);
    check("the_reaper_returns_a_deleted_task_s_stack", dynobj::the_reaper_returns_a_deleted_task_s_stack(), &mut pass, &mut fail);
    check("the_reaper_skips_a_task_a_core_is_on", dynobj::the_reaper_skips_a_task_a_core_is_on(), &mut pass, &mut fail);
    check("a_dynamic_queue_round_trips_on_hardware", dynobj::a_dynamic_queue_round_trips_on_hardware(), &mut pass, &mut fail);
    check("semaphores_and_event_bits_work_on_target", dynobj::semaphores_and_event_bits_work_on_target(), &mut pass, &mut fail);

    raw_print("[FLINT] SELFTEST END pass=");
    print_u32(pass);
    raw_print(" fail=");
    print_u32(fail);
    raw_print("\r\n");
}

/// Report a test that could not run here, without counting it either way.
///
/// A test that needs hardware this board has not got is not a pass and not a
/// failure. Counting it as a pass claims coverage that does not exist; as a
/// failure it trains people to ignore red. Dropping it silently is worse than
/// both, because the suite then reports a smaller total and nothing says why.
///
/// The line is deliberately not `PASS` or ` FAIL `, which is what keeps
/// `tools/target-test.sh` from counting it — that harness reconciles the
/// summary against the lines that actually arrived, so a skip must match
/// neither pattern.
fn skip(name: &str, reason: &str) {
    raw_print("[FLINT] TEST ");
    raw_print(name);
    raw_print(" SKIP ");
    raw_print(reason);
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

/// The same idea, but through **eight-register windows**, which is what the
/// two tests above never reach.
///
/// They both recurse directly, and LLVM compiles a direct self-call into a
/// `call4`. Only `WindowOverflow4` and `WindowUnderflow4` are ever exercised —
/// and those two were the only ones this kernel had right. The call8 and call12
/// handlers were wrong from the day the file was written and passed every test
/// here for a year, until a GCC-built radio blob, which uses call8 throughout,
/// double-faulted on every preemption.
///
/// So: recurse through a `fn` pointer, which LLVM has to emit as `callx8`, and
/// keep eight values live across the call so all of a4-a7 are in play. Deeper
/// than the 64-register file, so windows genuinely spill; long enough to be
/// preempted, so the trap frame lands in the middle of it — which is the exact
/// interaction the broken handler could not survive, because it spilled a4-a7
/// below the live stack pointer and the trap frame was written on top.
///
/// Checked by value, not by not-crashing. A misplaced spill returns wrong
/// arithmetic just as readily as it faults.
///
/// Confirmed to catch it, by putting the old `_WindowOverflow8` back and
/// running this suite: the board boot-loops and `make test-target` reports
/// that it never reached the self-tests. Loud rather than precise, which is
/// the right way round — but worth knowing that the failure arrives as a dead
/// board and not as this line printing FAIL.
fn call8_windows_survive_preemption() -> Check {
    // Kept out of line and behind a pointer so the compiler cannot turn the
    // recursion into a loop or a direct call.
    static DESCEND8: fn(u32, u32, u32, u32, u32, u32) -> u32 = descend8;

    fn descend8(n: u32, a: u32, b: u32, c: u32, d: u32, e: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        // Every argument is used *after* the recursive call, so each one has to
        // survive a spill and a restore rather than being dead across it.
        let deeper = core::hint::black_box(DESCEND8)(n - 1, a, b, c, d, e);
        deeper
            .wrapping_add(n)
            .wrapping_add(a ^ n)
            .wrapping_add(b.rotate_left(n % 32))
            .wrapping_add(c.wrapping_mul(n))
            .wrapping_add(d.wrapping_sub(n))
            .wrapping_add(e)
    }

    // Computed the same way, iteratively, so the expected value is not this
    // function's own output written down.
    fn expected(depth: u32, a: u32, b: u32, c: u32, d: u32, e: u32) -> u32 {
        let mut acc = 0u32;
        let mut n = 1;
        while n <= depth {
            acc = acc
                .wrapping_add(n)
                .wrapping_add(a ^ n)
                .wrapping_add(b.rotate_left(n % 32))
                .wrapping_add(c.wrapping_mul(n))
                .wrapping_add(d.wrapping_sub(n))
                .wrapping_add(e);
            n += 1;
        }
        acc
    }

    const DEPTH: u32 = 96;
    const A: u32 = 0x1234_5678;
    const B: u32 = 0x9ABC_DEF0;
    const C: u32 = 0x0F0F_0F0F;
    const D: u32 = 0xDEAD_BEEF;
    const E: u32 = 0xFEED_FACE;

    let want = expected(DEPTH, A, B, C, D, E);

    // Several passes, so the tick lands at a different depth each time. One
    // pass can get lucky about where the interrupt falls.
    let start = Tick::now();
    let mut rounds = 0u32;
    while Tick::now().saturating_sub(start) < 3 {
        if core::hint::black_box(DESCEND8)(DEPTH, A, B, C, D, E) != want {
            return Err("a call8 window came back corrupted");
        }
        rounds += 1;
        if rounds > 100_000 {
            break; // the tick is not running; tick_advances will say so
        }
    }
    if rounds == 0 {
        return Err("the recursion never ran");
    }
    Ok(())
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

/// Busy-wait `ticks` timer ticks. The coarse twin of [`spin_cycles`], for
/// waits long enough that the tick is the right unit — a pull settling, a
/// watchdog window.
///
/// Only for waiting. The loops that *measure* elapsed ticks keep their own
/// start value and do not call this: they look similar, but what they want is
/// the reading, not the delay.
#[cfg(target_os = "none")]
pub(crate) fn spin_ticks(ticks: u64) {
    use hal::tick::TickSource;
    let start = crate::arch::Tick::now();
    while crate::arch::Tick::now().saturating_sub(start) < ticks {
        core::hint::spin_loop();
    }
}
