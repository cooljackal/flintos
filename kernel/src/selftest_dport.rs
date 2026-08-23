// SPDX-License-Identifier: Apache-2.0

//! DPORT access self-tests. Included by [`crate::selftest`].
//!
//! These cannot run on a host. The erratum workaround in `soc_esp32::dport` is
//! four Xtensa instructions against a register block that exists only on the
//! chip; a host test can check the arithmetic around it and nothing else.
//!
//! What is provable here, single-core, is that the workaround did not break
//! ordinary reading. That is worth its own tests because the failure modes are
//! quiet: an asm block with the wrong operand constraints returns a plausible
//! number, and one that restores `PS` wrongly leaves interrupts masked — which
//! looks like a hung kernel several seconds later, nowhere near the cause.
//!
//! The cross-core half — two cores hammering DPORT at once — needs the second
//! core running, so it lives in `apps/tests/smp` where that is already true.

use hal::tick::TickSource;

use crate::arch::Tick;

use super::{spin_cycles, Check};

/// A clock bit safe to toggle: UART2 is not the console and no driver in the
/// self-test image claims it. Toggling UART0's would take the console out mid
/// test, which is a memorable way to learn this lesson.
#[cfg(target_os = "none")]
const SCRATCH: soc_esp32::dport::ClockBit = soc_esp32::dport::ClockBit::UART2;

/// The guarded read must return what the register actually holds.
///
/// With nothing else running, a plain read and a guarded read see the same
/// quiescent register. If the asm's operand constraints are wrong — the
/// classic being to return the APB pre-read instead of the DPORT value — this
/// is where it shows up, and it shows up as a specific wrong number rather
/// than a crash.
#[cfg(target_os = "none")]
pub(crate) fn dport_read_agrees_with_a_plain_read() -> Check {
    let reg = soc_esp32::dport::PERIP_CLK_EN;
    let guarded = unsafe { soc_esp32::dport::read(reg) };
    let plain = unsafe { (reg as *const u32).read_volatile() };
    if guarded != plain {
        return Err("guarded read disagreed with a plain read of a quiescent register");
    }
    // Zero would also satisfy the comparison above if both paths were broken
    // the same way. The console runs on UART0, so its clock bit is set by
    // definition while this message can be printed at all.
    if guarded & soc_esp32::dport::ClockBit::UART0.mask() == 0 {
        return Err("read returned a value with UART0 ungated, but the console is running");
    }
    Ok(())
}

/// Reading must not leave interrupts masked.
///
/// The read raises `PS.INTLEVEL` to 5 and restores it. A restore that writes
/// the wrong value, or is optimised away, leaves the tick masked — and the
/// symptom is a kernel that stops scheduling some time later, with nothing to
/// connect it back to a register read.
#[cfg(target_os = "none")]
pub(crate) fn dport_read_leaves_the_tick_running() -> Check {
    let reg = soc_esp32::dport::PERIP_CLK_EN;
    for _ in 0..64 {
        let _ = unsafe { soc_esp32::dport::read(reg) };
    }
    let before = Tick::now();
    // Long enough for at least one tick at any sane rate.
    spin_cycles(4_000_000);
    if Tick::now() == before {
        return Err("the tick stopped after a burst of DPORT reads — PS was not restored");
    }
    Ok(())
}

/// A modify must change its own bit and leave every other one alone.
///
/// This is the property the lock exists to preserve, checked here against the
/// real register rather than a variable. A read-modify-write that reads
/// garbage — the erratum's actual symptom — writes garbage back, and would
/// clear unrelated peripherals' clock bits. Including the console's.
#[cfg(target_os = "none")]
pub(crate) fn dport_modify_changes_only_its_own_bit() -> Check {
    use soc_esp32::dport;

    let reg = dport::PERIP_CLK_EN;
    let before = unsafe { dport::read(reg) };
    let started_set = before & SCRATCH.mask() != 0;

    unsafe { dport::enable(SCRATCH) };
    let enabled = unsafe { dport::read(reg) };
    if enabled & SCRATCH.mask() == 0 {
        return Err("enable did not set the clock bit");
    }
    if enabled & !SCRATCH.mask() != before & !SCRATCH.mask() {
        return Err("enable disturbed a bit that was not its own");
    }

    unsafe { dport::disable(SCRATCH) };
    let disabled = unsafe { dport::read(reg) };
    if disabled & SCRATCH.mask() != 0 {
        return Err("disable did not clear the clock bit");
    }
    if disabled & !SCRATCH.mask() != before & !SCRATCH.mask() {
        return Err("disable disturbed a bit that was not its own");
    }

    // Leave the chip as we found it. A self-test that gates a peripheral off
    // and walks away is a self-test that breaks whatever runs next.
    if started_set {
        unsafe { dport::enable(SCRATCH) };
    }
    Ok(())
}

// Host builds compile the module but have no register block to test. The
// stand-ins keep `selftest::run` one list rather than two behind cfg.
#[cfg(not(target_os = "none"))]
pub(crate) fn dport_read_agrees_with_a_plain_read() -> Check {
    Ok(())
}
#[cfg(not(target_os = "none"))]
pub(crate) fn dport_read_leaves_the_tick_running() -> Check {
    Ok(())
}
#[cfg(not(target_os = "none"))]
pub(crate) fn dport_modify_changes_only_its_own_bit() -> Check {
    Ok(())
}
