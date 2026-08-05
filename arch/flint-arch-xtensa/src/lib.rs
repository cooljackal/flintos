//! Xtensa LX6 architecture support for ESP32.
//!
//! Provides:
//! - Exception vectors and context save/restore assembly
//! - Boot startup (BSS init, data copy, watchdog disable)
//! - Syscall ABI (`XtensaSyscallABI`)
//! - Tick source (`XtensaTick`) via CCOUNT/CCOMPARE0
//! - MPU manager (`Esp32Mpu`) — Phase 2 stub
//! - Register-level accessors (`registers`)

#![no_std]
#![feature(asm_experimental_arch)]

core::arch::global_asm!(include_str!("asm/vectors.S"));
core::arch::global_asm!(include_str!("asm/context.S"));
core::arch::global_asm!(include_str!("startup.S"));

pub mod app_desc;
pub mod critical_section;
pub mod mpu;
pub mod registers;
pub mod syscall;
pub mod tick;

pub use critical_section::{with as cs_with, XtensaCriticalSection, XtensaCsToken};

// Assembly function prototypes (provided by context.S / vectors.S).
extern "C" {
    /// Spill all live register windows to their on-stack save areas.
    pub fn flint_spill_all_windows();

    /// Cooperative context switch: save `current`, restore `next`.
    pub fn flint_context_switch(
        current: *mut flint_hal::TaskContext,
        next: *const flint_hal::TaskContext,
    );

    /// Bootstrap the first task (no current context to save).
    pub fn flint_restore_first(next: *const flint_hal::TaskContext) -> !;
}
