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

    // Symmetric with `stall`, and for the same reason: this write crosses into
    // the RTC slow-clock domain, so the core does not resume when it retires
    // and `is_stalled` can still answer "stalled" afterwards. A second flash
    // operation soon after the first would then read a stale answer, skip the
    // stall it needed, and switch off the cache under a running core.
    let start = cycles();
    while cycles().wrapping_sub(start) < STALL_SETTLE_CYCLES {
        core::hint::spin_loop();
    }
}

/// Hold the APP CPU stalled.
///
/// The inverse of [`unstall`], and the pair encodes a magic value rather than
/// a flag: `0x21` in the C1 field and `0x2` in C0 is what the hardware reads
/// as stalled. Writing anything else leaves the core running.
///
/// # When this is the right instrument
///
/// esp-idf deliberately does **not** stall the other core for flash
/// operations while its scheduler is running; it uses a task handshake
/// instead, because stalling a core that holds a spinlock deadlocks whoever
/// wants that lock next.
///
/// FlintOS can stall, but the reason is narrower than it first looks, and the
/// obvious justification is wrong: the APP CPU *does* run kernel tasks and
/// *does* take the scheduler spinlock — `apps/smp` calls `join_scheduler` on
/// it. [`start`]'s doc comment still says otherwise and predates that.
///
/// What makes it safe is the caller, not the core: the flash driver takes no
/// lock between stalling and releasing. Core 1 may be stalled holding the
/// scheduler lock, and nothing on core 0 asks for it during that window.
///
/// **That is a property of the flash path, and a fragile one.** Add a lock
/// acquisition anywhere inside a flash operation and this deadlocks — core 0
/// waiting for a lock held by a core that cannot run. If that ever becomes
/// awkward to guarantee, the answer is NuttX's shape in
/// `esp32_spiflash_opstart`: signal the other core, wait for it to park
/// itself, and never stall hardware that might be holding something.
///
/// # Safety
/// Writes RTC registers. The caller must unstall: a core left stalled fetches
/// nothing and looks exactly like a core that crashed.
pub unsafe fn stall() {
    let opt = RTC_OPTIONS0 as *mut u32;
    crate::reg::modify(opt, SW_STALL_C0_MASK << SW_STALL_C0_SHIFT, 0x2 << SW_STALL_C0_SHIFT);

    let sw = RTC_SW_CPU_STALL as *mut u32;
    crate::reg::modify(sw, SW_STALL_C1_MASK << SW_STALL_C1_SHIFT, 0x21 << SW_STALL_C1_SHIFT);

    // **The write does not take effect when it retires.** These two registers
    // live in RTC_CNTL, which runs on the RTC slow clock -- roughly 150 kHz,
    // against a CPU at 80 or 240 MHz -- so the request crosses a clock domain
    // and the other core keeps executing for some microseconds afterwards.
    //
    // Returning immediately is what made this a bug rather than a race nobody
    // hit: `esp32_flash::with_cache_off` stalls the APP CPU and then disables
    // its cache, and for that window the core was still fetching through a
    // cache that had gone away. It crashed with `EXCCAUSE=0` (illegal
    // instruction) inside `Scheduler::schedule` -- core 1 executing rubbish,
    // reported from core 0's fault handler. Timing-dependent, so it survived a
    // run on one board and failed on the first run on another.
    //
    // There is no "stalled" status bit to poll, so this waits. A few RTC slow
    // cycles is the requirement; at ~150 kHz one cycle is ~6.7 us, and the
    // budget below is 32768 CPU cycles -- 136 us at 240 MHz, 410 us at 80 MHz,
    // so tens of slow cycles either way. Against a sector erase of tens of
    // milliseconds it does not register.
    //
    // esp-idf reaches for an IPI and a spin handshake (`esp_ipc_isr_stall_other_cpu`)
    // rather than this register for exactly this reason, and keeps the RTC
    // stall for the panic path where a delay is free. That handshake is still
    // the better answer here; see `esp32_flash`'s module docs.
    let start = cycles();
    while cycles().wrapping_sub(start) < STALL_SETTLE_CYCLES {
        core::hint::spin_loop();
    }
}

/// How long [`stall`] waits for the RTC domain to catch up, in CPU cycles.
const STALL_SETTLE_CYCLES: u32 = 32_768;

/// `CCOUNT`, the cycle counter.
///
/// Read here rather than through `arch-xtensa` because a `soc/*` crate may
/// depend only on `hal` — `make check-layers` enforces it.
#[inline(always)]
fn cycles() -> u32 {
    #[cfg(target_arch = "xtensa")]
    {
        let v: u32;
        unsafe { core::arch::asm!("rsr.ccount {0}", out(reg) v) };
        v
    }
    // No cycle counter on a host, and nothing to stall either. Returning a
    // constant makes the wait above terminate immediately, which is right:
    // `stall` on a host is already a no-op against real hardware.
    #[cfg(not(target_arch = "xtensa"))]
    {
        u32::MAX
    }
}

/// Release the stall. Public counterpart to the private helper used by
/// [`start`], for callers that stalled with [`stall`].
///
/// # Safety
/// Writes RTC registers.
pub unsafe fn unstall_now() {
    unsafe { unstall() }
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
/// **Historically not the kernel**, and this paragraph is now out of date:
/// `kernel::boot::join_scheduler` brings the APP CPU into the scheduler, and
/// `apps/smp` does exactly that. What follows described the state before that
/// existed and is kept because the constraints it lists still apply to a core
/// started *without* joining.
///
/// **Not the kernel.** FlintOS's scheduler is single-core: `scheduler::global()`
/// hands out `&mut` to a `static`, and a critical section masks interrupts on
/// the calling core only. Two cores in that code is a data race in the
/// language's own terms, not merely a logical one.
///
/// So `entry` may use its own stack and memory it owns, and may read or write
/// `AtomicU32`s shared with the PRO CPU. It must not call into `kernel`, take a
/// FlintOS mutex, send on a FlintOS queue, or spawn a task. Making any of that safe
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
