// SPDX-License-Identifier: Apache-2.0

//! Xtensa LX6 register definitions and inline accessors.
//!
//! Interrupt control on Xtensa is via **CPU special registers** (`rsr`/`wsr`),
//! not memory-mapped DPORT addresses. (The earlier version modelled
//! INTENABLE/INTCLEAR as DPORT addresses — that was wrong; see plan W0.5.)
//! The DPORT interrupt *matrix* — which routes peripheral sources to CPU
//! interrupt numbers — is separate and kept under [`dport`].

// ── Processor State (PS) ─────────────────────────────────────────────────────

pub const PS_WOE: u32 = 1 << 18;     // Window Overflow Enable (PS.WOE is bit 18)
pub const PS_EXCM: u32 = 1 << 4;     // Exception mode (PS.EXCM is bit 4)
pub const PS_INTLEVEL_SHIFT: u32 = 0;
pub const PS_INTLEVEL_MASK: u32 = 0xF; // PS.INTLEVEL is bits 0..3
pub const PS_UM: u32 = 1 << 5;       // User vector mode (PS.UM is bit 5)
pub const PS_CALLINC_SHIFT: u32 = 16;
pub const PS_CALLINC_MASK: u32 = 3 << PS_CALLINC_SHIFT;
pub const PS_OWB_SHIFT: u32 = 8;
pub const PS_OWB_MASK: u32 = 0xF << PS_OWB_SHIFT;

// ── Internal interrupt assignments (ESP32, PRO CPU) ──────────────────────────

/// CPU-internal Timer0 (CCOMPARE0) interrupt number. Level-1.
pub const INT_TIMER0: u32 = 6;
/// Mask for the Timer0 interrupt in INTENABLE / INTERRUPT.
pub const INT_TIMER0_MASK: u32 = 1 << INT_TIMER0;

/// CPU-internal software interrupt number (level-1). Used by the kernel to
/// request a context switch from task context (cooperative yield/sleep/block):
/// raising it makes the switch happen in the trap handler, so every switch
/// resumes via the same `rfe` path.
pub const INT_SOFTWARE: u32 = 7;
/// Mask for the software interrupt.
pub const INT_SOFTWARE_MASK: u32 = 1 << INT_SOFTWARE;

// ── DPORT interrupt matrix (peripheral source → CPU interrupt routing) ───────

// `dport` used to live here: the ESP32 interrupt crossbar, at 0x3FF00000.
// Deleted rather than moved. It was chip infrastructure inside the CPU-core
// crate, it had no callers, and `soc_esp32::intr_map` is the live
// implementation of the same registers -- written later, without noticing this
// one, which is exactly what a duplicate in the wrong tier costs.
//
// `rtc_cntl` was the same mistake and is now gone the same way. It was here
// because `tick.rs` measured the CPU frequency against the RTC slow clock,
// which meant this crate -- whose whole subject is the Xtensa LX6, a core the
// ESP32 merely uses -- carrying an ESP32 peripheral's base address and offsets.
// `soc_esp32::rtc` owns those, and `TickSource::init` now takes `cpu_hz` from
// its caller: the kernel is the one crate that may name both an arch and a
// SoC, so the kernel does the measuring.

// ── PS access ────────────────────────────────────────────────────────────────

/// Read PS.
///
/// # Safety
/// Reads `PS`. No side effects.
pub unsafe fn read_ps() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.ps {0}", out(reg) val);
    val
}

/// Point this core at a vector table.
///
/// Per-core: each core has its own `VECBASE`, and a core that never sets one
/// takes every exception to wherever reset left it. The two cores can share
/// the same table — it is code, and the handler is written to work on either.
///
/// # Safety
/// `base` must point at a valid vector table, 1 KiB aligned. Wrong, and the
/// first interrupt on this core goes somewhere arbitrary.
#[inline(always)]
pub unsafe fn set_vecbase(base: u32) {
    core::arch::asm!("wsr.vecbase {0}", "rsync", in(reg) base, options(nostack));
}

/// Read VECBASE, the base of this core's exception vector table.
///
/// Bring-up diagnostics only: it should equal `_vector_table_start` once
/// `startup.S` has run, which proves the table was installed rather than left
/// at the ROM's own vectors.
///
/// # Safety
/// Reads `VECBASE`. No side effects.
pub unsafe fn read_vecbase() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.vecbase {0}", out(reg) val);
    val
}

/// Read the current stack pointer (`a1`). Diagnostic-only: by the time any
/// Rust code can call this the window has already rotated at least once, so
/// this reads whatever `a1` denotes in the *caller's* live window, which is
/// exactly the stack pointer bring-up diagnostics want to report.
///
/// # Safety
/// Reads `a1`. No side effects.
pub unsafe fn read_sp() -> u32 {
    let val: u32;
    core::arch::asm!("mov {0}, a1", out(reg) val);
    val
}

// ── Cycle counter / tick timer ───────────────────────────────────────────────

/// Read the CCOUNT special register (free-running cycle counter).
///
/// # Safety
/// Reads the cycle counter. No side effects.
pub unsafe fn read_ccount() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.ccount {0}", out(reg) val);
    val
}

/// Read CCOMPARE0.
///
/// # Safety
/// Reads `CCOMPARE0`. No side effects.
pub unsafe fn read_ccompare0() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.ccompare0 {0}", out(reg) val);
    val
}

/// Write CCOMPARE0. Writing CCOMPARE0 also clears a pending Timer0 (CCOMPARE0)
/// interrupt — this is how the tick is acknowledged and re-armed.
///
/// # Safety
/// Arms the core timer. A value already behind `CCOUNT` does not fire for a full wrap.
pub unsafe fn set_ccompare0(val: u32) {
    core::arch::asm!("wsr.ccompare0 {0}", "rsync", in(reg) val);
}

// ── Interrupt special registers ──────────────────────────────────────────────

/// Read EXCCAUSE (cause of the current exception). For a level-1 interrupt the
/// cause is `EXCCAUSE_LEVEL1_INTERRUPT` (4); genuine exceptions use other codes.
///
/// # Safety
/// Reads `EXCCAUSE`. Meaningful only inside a trap handler.
pub unsafe fn read_exccause() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.exccause {0}", out(reg) val);
    val
}

/// EXCCAUSE value for a level-1 interrupt taken via the user/kernel vector.
pub const EXCCAUSE_LEVEL1_INTERRUPT: u32 = 4;

/// Read EXCVADDR (faulting data address for load/store errors).
///
/// # Safety
/// Reads `EXCVADDR`. Meaningful only inside a trap handler.
pub unsafe fn read_excvaddr() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.excvaddr {0}", out(reg) val);
    val
}

/// Read INTERRUPT (pending interrupts, special register).
///
/// # Safety
/// Reads `INTERRUPT`. No side effects.
/// In IRAM: read by the trap handler's cache-off path, where a call into
/// flash does not fault, it simply stops the core.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.regs")]
pub unsafe fn read_interrupt() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.interrupt {0}", out(reg) val);
    val
}

/// Read INTENABLE (enabled interrupt mask, special register).
///
/// One bit per CPU interrupt, and the finer instrument beside `PS.INTLEVEL`:
/// raising the level masks everything below it, while clearing a bit here
/// masks one interrupt and leaves the rest live.
///
/// That distinction is what lets a flash operation keep servicing handlers
/// safe to run with the instruction cache off, rather than stopping the world
/// for the tens of milliseconds a sector erase takes. esp-idf
/// (`esp_intr_noniram_disable`) and NuttX (`esp32_spiflash_opstart`) both work
/// this way; see `kernel::interrupt::mask_non_iram_safe`.
///
/// # Safety
/// Reads `INTENABLE`. No side effects.
/// In IRAM: read by the trap handler's cache-off path, where a call into
/// flash does not fault, it simply stops the core.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.regs")]
pub unsafe fn read_intenable() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.intenable {0}", out(reg) val);
    val
}

/// Write INTENABLE.
///
/// # Safety
/// Replaces the whole interrupt mask for this core. Masking a handler the kernel depends on is silent -- the peripheral simply stops being serviced.
pub unsafe fn write_intenable(val: u32) {
    core::arch::asm!("wsr.intenable {0}", "rsync", in(reg) val);
}

/// Enable a CPU interrupt by number (set its bit in INTENABLE).
///
/// # Safety
/// Read-modify-write of `INTENABLE`, so it must not race another writer on this core; the caller holds a critical section.
pub unsafe fn enable_interrupt(num: u32) {
    let cur = read_intenable();
    write_intenable(cur | (1 << num));
}

/// Clear an edge/software interrupt via INTCLEAR (special register, write-only).
/// NOTE: the Timer0 (CCOMPARE0) interrupt is *not* cleared here — it is cleared
/// by re-writing CCOMPARE0 (see [`set_ccompare0`]).
///
/// # Safety
/// Clears pending interrupt bits. Clearing one that has not been handled loses it.
pub unsafe fn intclear(mask: u32) {
    core::arch::asm!("wsr.intclear {0}", "rsync", in(reg) mask);
}

/// Raise a software interrupt via INTSET (special register, write-only).
///
/// # Safety
/// Raises interrupts in software. The corresponding handler must be installed.
pub unsafe fn intset(mask: u32) {
    core::arch::asm!("wsr.intset {0}", "rsync", in(reg) mask);
}

/// Request a context switch by raising the software interrupt.
///
/// # Safety
/// Raises the software interrupt the scheduler switches on. Safe to call from any context; the switch happens on the way out of the handler.
pub unsafe fn request_switch() {
    intset(INT_SOFTWARE_MASK);
}

// ── Window state ─────────────────────────────────────────────────────────────

/// # Safety
/// Reads `WINDOWBASE`. No side effects.
pub unsafe fn read_windowbase() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.windowbase {0}", out(reg) val);
    val
}

/// # Safety
/// Reads `WINDOWSTART`. No side effects.
pub unsafe fn read_windowstart() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.windowstart {0}", out(reg) val);
    val
}

// ── Interrupt level (PS.INTLEVEL) helpers ────────────────────────────────────

/// Set interrupt level to 0 (enable all interrupts), returning previous PS.
///
/// # Safety
/// Unmasks every level. Only correct where the caller knows interrupts should be on, such as the end of boot.
#[inline]
pub unsafe fn set_intlevel_0() -> u32 {
    let prev: u32;
    core::arch::asm!("rsil {0}, 0", out(reg) prev);
    prev
}

/// Set interrupt level to 15 (disable all maskable interrupts), returning prev PS.
///
/// The returned PS has no restorer here on purpose. Its one caller is the
/// panic path, which never comes back, and the two functions that used to sit
/// beside this one -- `write_ps` and `restore_ps`, identical bodies 166 lines
/// apart -- had no callers at all. Code that needs to restore PS should use
/// [`crate::critical_section`], which pairs the two by construction.
///
/// # Safety
/// Masks every maskable interrupt on this core, and nothing here puts them
/// back. Only correct on a path that does not return.
#[inline]
pub unsafe fn set_intlevel_15() -> u32 {
    let prev: u32;
    core::arch::asm!("rsil {0}, 15", out(reg) prev);
    prev
}
