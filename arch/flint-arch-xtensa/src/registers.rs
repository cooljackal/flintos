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

pub mod dport {
    /// DPORT base.
    pub const BASE: u32 = 0x3FF0_0000;
    /// PRO-CPU peripheral interrupt map base (DPORT_PRO_*_MAP_REG).
    /// Each peripheral source has a 32-bit register at BASE + 0x104 + src*4
    /// holding the CPU interrupt number it is routed to.
    pub const PRO_MAP_BASE: u32 = BASE + 0x104;

    /// Route peripheral interrupt `source` to CPU interrupt `cpu_int`.
    pub unsafe fn route(source: u32, cpu_int: u32) {
        let reg = (PRO_MAP_BASE + source * 4) as *mut u32;
        core::ptr::write_volatile(reg, cpu_int);
    }
}

// ── RTC_CNTL (used only to measure the CPU clock at boot) ────────────────────

pub mod rtc_cntl {
    //! RTC timer registers, used solely by `tick::XtensaTick::init` to
    //! measure the actual CPU clock against the RTC slow clock (issue #6) --
    //! Flint does not otherwise touch the clock tree.
    //!
    //! VERIFIED against the espressif/esp-idf `master` branch (fetched
    //! 2026-08-05):
    //!   - `components/soc/esp32/register/soc/reg_base.h`:
    //!     `DR_REG_RTCCNTL_BASE = 0x3FF4_8000`
    //!   - `components/soc/esp32/register/soc/rtc_cntl_reg.h`:
    //!     `RTC_CNTL_TIME_UPDATE_REG = DR_REG_RTCCNTL_BASE + 0x0C`
    //!     `RTC_CNTL_TIME0_REG       = DR_REG_RTCCNTL_BASE + 0x10`
    //!     `RTC_CNTL_TIME1_REG       = DR_REG_RTCCNTL_BASE + 0x14`
    //!     `RTC_CNTL_TIME_UPDATE = BIT(31)` (write-1 triggers a snapshot),
    //!     `RTC_CNTL_TIME_VALID  = BIT(30)` (read-only, set once the
    //!     snapshot has landed in TIME0/TIME1).
    //!   - The read sequence (write TIME_UPDATE, poll TIME_VALID, then read
    //!     TIME0 as the low 32 bits / TIME1 as the high bits) matches
    //!     ESP-IDF's own `rtc_time_get()` (`esp_hw_support/port/esp32/
    //!     rtc_time.c` and predecessors). We skip ESP-IDF's follow-up write
    //!     to `RTC_CNTL_INT_CLR_REG`: that only clears a stale "time valid"
    //!     RTC interrupt flag, and Flint never enables that interrupt, so
    //!     there is nothing to clear.
    //!
    //! UNVERIFIED: the RTC slow-clock source and its nominal rate (150 kHz
    //! internal RC oscillator by default, per the ESP32 TRM) were taken on
    //! the task's word rather than independently re-derived from a register
    //! read of `RTC_CNTL_CLK_CONF_REG`. Flint's boot path never switches the
    //! slow-clock source, so the reset default should hold, but this is the
    //! one link in the chain not directly confirmed against a header dump.

    /// RTC_CNTL peripheral base.
    pub const BASE: u32 = 0x3FF4_8000;

    const TIME_UPDATE_REG: *mut u32 = (BASE + 0x0C) as *mut u32;
    const TIME0_REG: *const u32 = (BASE + 0x10) as *const u32;
    const TIME1_REG: *const u32 = (BASE + 0x14) as *const u32;

    const TIME_UPDATE: u32 = 1 << 31;
    const TIME_VALID: u32 = 1 << 30;

    /// Trigger an RTC counter snapshot and read it back once valid.
    ///
    /// Returns `None` if `TIME_VALID` has not set after `max_polls` reads of
    /// `TIME_UPDATE_REG`, so a missing/stuck RTC block can't hang boot.
    pub unsafe fn read_counter(max_polls: u32) -> Option<u64> {
        core::ptr::write_volatile(TIME_UPDATE_REG, TIME_UPDATE);
        let mut polls = 0u32;
        while core::ptr::read_volatile(TIME_UPDATE_REG) & TIME_VALID == 0 {
            polls += 1;
            if polls > max_polls {
                return None;
            }
        }
        let lo = core::ptr::read_volatile(TIME0_REG) as u64;
        let hi = core::ptr::read_volatile(TIME1_REG) as u64;
        Some((hi << 32) | lo)
    }
}

// ── PS access ────────────────────────────────────────────────────────────────

/// Read PS.
pub unsafe fn read_ps() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.ps {0}", out(reg) val);
    val
}

/// Write PS.
pub unsafe fn write_ps(val: u32) {
    core::arch::asm!("wsr.ps {0}", "rsync", in(reg) val);
}

/// Read VECBASE (base address of the exception vector table). Used only for
/// bring-up diagnostics: it should equal `_vector_table_start` once
/// `startup.S` has run, proving the vector table was actually installed
/// rather than left at the ROM's own vectors.
pub unsafe fn read_vecbase() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.vecbase {0}", out(reg) val);
    val
}

/// Read the current stack pointer (`a1`). Diagnostic-only: by the time any
/// Rust code can call this the window has already rotated at least once, so
/// this reads whatever `a1` denotes in the *caller's* live window, which is
/// exactly the stack pointer bring-up diagnostics want to report.
pub unsafe fn read_sp() -> u32 {
    let val: u32;
    core::arch::asm!("mov {0}, a1", out(reg) val);
    val
}

// ── Cycle counter / tick timer ───────────────────────────────────────────────

/// Read the CCOUNT special register (free-running cycle counter).
pub unsafe fn read_ccount() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.ccount {0}", out(reg) val);
    val
}

/// Read CCOMPARE0.
pub unsafe fn read_ccompare0() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.ccompare0 {0}", out(reg) val);
    val
}

/// Write CCOMPARE0. Writing CCOMPARE0 also clears a pending Timer0 (CCOMPARE0)
/// interrupt — this is how the tick is acknowledged and re-armed.
pub unsafe fn set_ccompare0(val: u32) {
    core::arch::asm!("wsr.ccompare0 {0}", "rsync", in(reg) val);
}

// ── Interrupt special registers ──────────────────────────────────────────────

/// Read EXCCAUSE (cause of the current exception). For a level-1 interrupt the
/// cause is `EXCCAUSE_LEVEL1_INTERRUPT` (4); genuine exceptions use other codes.
pub unsafe fn read_exccause() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.exccause {0}", out(reg) val);
    val
}

/// EXCCAUSE value for a level-1 interrupt taken via the user/kernel vector.
pub const EXCCAUSE_LEVEL1_INTERRUPT: u32 = 4;

/// Read EXCVADDR (faulting data address for load/store errors).
pub unsafe fn read_excvaddr() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.excvaddr {0}", out(reg) val);
    val
}

/// Read INTERRUPT (pending interrupts, special register).
pub unsafe fn read_interrupt() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.interrupt {0}", out(reg) val);
    val
}

/// Read INTENABLE (enabled interrupt mask, special register).
pub unsafe fn read_intenable() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.intenable {0}", out(reg) val);
    val
}

/// Write INTENABLE.
pub unsafe fn write_intenable(val: u32) {
    core::arch::asm!("wsr.intenable {0}", "rsync", in(reg) val);
}

/// Enable a CPU interrupt by number (set its bit in INTENABLE).
pub unsafe fn enable_interrupt(num: u32) {
    let cur = read_intenable();
    write_intenable(cur | (1 << num));
}

/// Clear an edge/software interrupt via INTCLEAR (special register, write-only).
/// NOTE: the Timer0 (CCOMPARE0) interrupt is *not* cleared here — it is cleared
/// by re-writing CCOMPARE0 (see [`set_ccompare0`]).
pub unsafe fn intclear(mask: u32) {
    core::arch::asm!("wsr.intclear {0}", "rsync", in(reg) mask);
}

/// Raise a software interrupt via INTSET (special register, write-only).
pub unsafe fn intset(mask: u32) {
    core::arch::asm!("wsr.intset {0}", "rsync", in(reg) mask);
}

/// Request a context switch by raising the software interrupt.
pub unsafe fn request_switch() {
    intset(INT_SOFTWARE_MASK);
}

// ── Window state ─────────────────────────────────────────────────────────────

pub unsafe fn read_windowbase() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.windowbase {0}", out(reg) val);
    val
}

pub unsafe fn read_windowstart() -> u32 {
    let val: u32;
    core::arch::asm!("rsr.windowstart {0}", out(reg) val);
    val
}

// ── Interrupt level (PS.INTLEVEL) helpers ────────────────────────────────────

/// Set interrupt level to 0 (enable all interrupts), returning previous PS.
#[inline]
pub unsafe fn set_intlevel_0() -> u32 {
    let prev: u32;
    core::arch::asm!("rsil {0}, 0", out(reg) prev);
    prev
}

/// Set interrupt level to 15 (disable all maskable interrupts), returning prev PS.
#[inline]
pub unsafe fn set_intlevel_15() -> u32 {
    let prev: u32;
    core::arch::asm!("rsil {0}, 15", out(reg) prev);
    prev
}

/// Restore a previously-saved PS interrupt level.
#[inline]
pub unsafe fn restore_ps(prev: u32) {
    core::arch::asm!("wsr.ps {0}", "rsync", in(reg) prev);
}
