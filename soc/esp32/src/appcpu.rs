// SPDX-License-Identifier: Apache-2.0

//! Starting the second core.
//!
//! The ESP32 has two Xtensa LX6 cores. Only PRO (core 0) runs at reset; APP
//! (core 1) sits held in reset, clock-gated, and stalled by the RTC — three
//! separate mechanisms, all of which must be released, in order.
//!
//! # What this does not do
//!
//! It starts a core. It does not make the kernel SMP-safe, and the two are
//! very different jobs. See [`start`] for exactly what the started core may
//! and may not touch.
//!
//! # The three holds
//!
//! Missing any one of them gives a core that never runs, with nothing
//! reporting a problem:
//!
//! | Hold | Register | Released by |
//! |---|---|---|
//! | RTC stall | `RTC_CNTL_OPTIONS0` bits [1:0], `RTC_CNTL_SW_CPU_STALL` bits [25:20] | clearing both fields |
//! | Clock gate | `DPORT_APPCPU_CTRL_B` bit 0 | setting it |
//! | Reset | `DPORT_APPCPU_CTRL_A` bit 0 | pulsing it |
//!
//! The stall is the one that catches people: it is in the RTC domain, not
//! DPORT, and it survives a DPORT reset. A core released from reset and clock
//! but left stalled fetches nothing.
//!
//! Register facts from `soc/dport_reg.h` and `soc/rtc_cntl_reg.h`.

use crate::addr::{DPORT_BASE, RTC_CNTL_BASE};

/// `DPORT_APPCPU_CTRL_A_REG`. Bit 0 holds the core in reset.
const APPCPU_CTRL_A: u32 = DPORT_BASE + 0x02C;
const APPCPU_RESETTING: u32 = 1 << 0;

/// `DPORT_APPCPU_CTRL_B_REG`. Bit 0 gates the core's clock.
const APPCPU_CTRL_B: u32 = DPORT_BASE + 0x030;
const APPCPU_CLKGATE_EN: u32 = 1 << 0;

/// `DPORT_APPCPU_CTRL_C_REG`. Bit 0 stalls the core from DPORT's side.
const APPCPU_CTRL_C: u32 = DPORT_BASE + 0x034;
const APPCPU_RUNSTALL: u32 = 1 << 0;

/// `DPORT_APPCPU_CTRL_D_REG`. The address the core fetches from on release.
const APPCPU_CTRL_D: u32 = DPORT_BASE + 0x038;

/// `RTC_CNTL_OPTIONS0_REG`, `SW_STALL_APPCPU_C0` in bits [1:0].
const RTC_OPTIONS0: u32 = RTC_CNTL_BASE;
const SW_STALL_C0_SHIFT: u32 = 0;
const SW_STALL_C0_MASK: u32 = 0x3;

/// ROM `Cache_Flush(int cpu)`, from `esp32.rom.ld`.
const CACHE_FLUSH_ROM: usize = 0x4000_9A14;
/// ROM `Cache_Read_Enable(int cpu)`, from `esp32.rom.ld`.
const CACHE_READ_ENABLE_ROM: usize = 0x4000_9A84;

/// `RTC_CNTL_SW_CPU_STALL_REG`, `SW_STALL_APPCPU_C1` in bits [25:20].
const RTC_SW_CPU_STALL: u32 = RTC_CNTL_BASE + 0xAC;
const SW_STALL_C1_SHIFT: u32 = 20;
const SW_STALL_C1_MASK: u32 = 0x3F;

/// Release the RTC's software stall on the APP CPU.
///
/// Two fields in two registers, and the core runs only when *both* are clear.
/// The pair encodes a magic value — 0x21 in C1 and 0x2 in C0 means stalled —
/// so clearing either is enough in practice, and clearing both is what esp-idf
/// does and what survives someone else setting one later.
///
/// # Safety
/// Writes RTC registers.
unsafe fn unstall() {
    let opt = RTC_OPTIONS0 as *mut u32;
    crate::reg::clear(opt, SW_STALL_C0_MASK << SW_STALL_C0_SHIFT);

    let sw = RTC_SW_CPU_STALL as *mut u32;
    crate::reg::clear(sw, SW_STALL_C1_MASK << SW_STALL_C1_SHIFT);
}

/// Whether the APP CPU is currently held stalled by the RTC.
///
/// # Safety
/// Reads RTC registers. No side effects.
pub unsafe fn is_stalled() -> bool {
    let c0 = ((RTC_OPTIONS0 as *const u32).read_volatile() >> SW_STALL_C0_SHIFT) & SW_STALL_C0_MASK;
    let c1 =
        ((RTC_SW_CPU_STALL as *const u32).read_volatile() >> SW_STALL_C1_SHIFT) & SW_STALL_C1_MASK;
    c0 != 0 || c1 != 0
}

/// Start the APP CPU at `entry`.
///
/// # What the started core may touch
///
/// **Not the kernel.** Flint's scheduler is single-core: `scheduler::global()`
/// hands out `&mut` to a `static`, and a critical section masks interrupts on
/// the calling core only. Two cores in that code is a data race in the
/// language's own terms, not merely a logical one.
///
/// So `entry` may use its own stack and memory it owns, and may read or write
/// `AtomicU32`s shared with the PRO CPU. It must not call into `kernel`, take a
/// Flint mutex, send on a Flint queue, or spawn a task. Making any of that safe
/// is the rest of #19, and it is a much larger job than this function.
///
/// # Safety
/// `entry` must never return — there is no context to return to. It must set
/// its own stack pointer before touching memory: the core arrives with `a1`
/// undefined, and the trampoline in `arch-xtensa` is what handles that.
///
/// Starting an already-running core resets it mid-instruction.
pub unsafe fn start(entry: unsafe extern "C" fn() -> !) {
    // The APP CPU's instruction cache, enabled from *this* core before the
    // other one is released. esp-idf does the same, in the same order, in
    // `start_other_core`.
    //
    // Without it the second core cannot fetch anything mapped from flash, and
    // that is not a corner case: every task, every driver and most of the
    // kernel live there. The symptom is not an error — the core faults on its
    // first flash instruction with no vector table to report it, or simply
    // stops. Both were seen before this call existed.
    //
    // Flush before enable. A stale cache enabled over new flash contents
    // serves whatever was there at the last boot.
    let flush: extern "C" fn(u32) = core::mem::transmute(CACHE_FLUSH_ROM);
    let read_enable: extern "C" fn(u32) = core::mem::transmute(CACHE_READ_ENABLE_ROM);
    flush(1);
    read_enable(1);

    // Order matters. Address first, so the core has somewhere to go the
    // instant it is released; a core released with CTRL_D still zero fetches
    // from 0 and takes an exception with no handler installed.
    crate::dport::write(APPCPU_CTRL_D, entry as usize as u32);

    // Ungate the clock.
    crate::dport::modify(APPCPU_CTRL_B, 0, APPCPU_CLKGATE_EN);

    // Release DPORT's stall.
    crate::dport::modify(APPCPU_CTRL_C, APPCPU_RUNSTALL, 0);

    // And the RTC's, which is a different mechanism in a different domain.
    unstall();

    // Pulse reset. The core begins fetching from CTRL_D on the falling edge.
    crate::dport::modify(APPCPU_CTRL_A, 0, APPCPU_RESETTING);
    crate::dport::modify(APPCPU_CTRL_A, APPCPU_RESETTING, 0);
}

/// Put the APP CPU back in reset.
///
/// # Safety
/// Whatever it was executing stops mid-instruction. Anything it was holding —
/// a lock, a half-written buffer — stays in that state.
pub unsafe fn stop() {
    crate::dport::modify(APPCPU_CTRL_A, 0, APPCPU_RESETTING);
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_control_registers_are_where_dport_reg_says() {
        assert_eq!(APPCPU_CTRL_A, 0x3FF0_002C);
        assert_eq!(APPCPU_CTRL_B, 0x3FF0_0030);
        assert_eq!(APPCPU_CTRL_C, 0x3FF0_0034);
        assert_eq!(APPCPU_CTRL_D, 0x3FF0_0038);
    }

    #[test]
    fn the_stall_fields_are_in_two_different_registers() {
        // The trap: one is in RTC_CNTL_OPTIONS0 and the other 0xAC further on.
        // Releasing only the DPORT runstall leaves the core fetching nothing,
        // with no register anywhere reporting a fault.
        assert_eq!(RTC_OPTIONS0, 0x3FF4_8000);
        assert_eq!(RTC_SW_CPU_STALL, 0x3FF4_80AC);
        assert_ne!(RTC_OPTIONS0, RTC_SW_CPU_STALL);
    }

    #[test]
    fn the_stall_masks_match_the_header_widths() {
        // SW_STALL_APPCPU_C0 is 2 bits at 0; C1 is 6 bits at 20. A mask that
        // is too wide clears neighbouring RTC configuration -- brownout and
        // clock settings live in the same registers.
        assert_eq!(SW_STALL_C0_MASK, 0x3);
        assert_eq!(SW_STALL_C0_SHIFT, 0);
        assert_eq!(SW_STALL_C1_MASK, 0x3F);
        assert_eq!(SW_STALL_C1_SHIFT, 20);
        // The C1 field must not run off the top of the register. Both sides
        // are constants, so this belongs at compile time.
        const _: () = assert!(SW_STALL_C1_SHIFT + 6 <= 32);
    }

    #[test]
    fn the_three_holds_are_three_different_bits() {
        // All are bit 0, but of three different registers. Collapsing them
        // into one write releases whichever register was written last.
        assert_eq!(APPCPU_RESETTING, 1);
        assert_eq!(APPCPU_CLKGATE_EN, 1);
        assert_eq!(APPCPU_RUNSTALL, 1);
        assert_ne!(APPCPU_CTRL_A, APPCPU_CTRL_B);
        assert_ne!(APPCPU_CTRL_B, APPCPU_CTRL_C);
    }
}
