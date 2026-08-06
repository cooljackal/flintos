// SPDX-License-Identifier: Apache-2.0

//! Xtensa LX6 architecture support for ESP32.
//!
//! Provides:
//! - Exception vectors and context save/restore assembly
//! - Boot startup (BSS init, data copy, watchdog disable)
//! - Tick source (`XtensaTick`) via CCOUNT/CCOMPARE0
//! - MPU manager (`Esp32Mpu`) — Phase 2 stub
//! - Register-level accessors (`registers`)

#![no_std]
#![feature(asm_experimental_arch)]

// The assembly sources are NOT included here with `global_asm!`. They are
// assembled by `build.rs` with `xtensa-esp32-elf-gcc` and linked as a static
// archive. LLVM's integrated assembler, which `global_asm!` routes through,
// rejects the windowed-register instructions (`s32e`, `l32e`, `rfwo`, `rfwu`)
// that the exception vectors are built from, so this crate could not be
// compiled for its own target. See build.rs.

pub mod app_desc;
pub mod critical_section;
pub mod mpu;
pub mod registers;
pub mod tick;

pub use critical_section::{with as cs_with, XtensaCriticalSection, XtensaCsToken};

// ── Trap frame geometry ─────────────────────────────────────────────────────
//
// These describe what `_flint_trap_entry` in vectors.S does to the interrupted
// task's stack. They exist to be asserted against: the assembly writes the
// frame by fixed offset, and nothing in the compiler connects those offsets to
// the Rust struct they are offsets *into*.

/// Size of the frame the trap entry builds. This is `TaskContext` itself — the
/// entry stores each field at its `#[repr(C)]` offset.
pub const TRAP_FRAME_BYTES: usize = core::mem::size_of::<hal::TaskContext>();

/// Bytes below a frame's stack pointer that the Xtensa windowed ABI reserves
/// for the caller's `a0`-`a3` when a window overflows.
///
/// The trap entry has to skip past this rather than write over it: the entry
/// spills all live windows *before* it moves the stack pointer, so a caller's
/// registers are already sitting there by the time the frame is allocated.
pub const ABI_SAVE_AREA_BYTES: usize = 16;

/// Total the trap entry subtracts from the interrupted task's stack pointer.
pub const TRAP_STACK_BYTES: usize = TRAP_FRAME_BYTES + ABI_SAVE_AREA_BYTES;

const _: () = {
    // vectors.S spells this out as a literal in three places: the scratch frame
    // pointer, the `addi a1, a1, -112`, and the `addi a0, a1, 112` that
    // recovers the original stack pointer. If TaskContext grows, this fires and
    // says where to look — which beats the alternative, where the trap entry
    // silently writes two fields into the register save area it was supposed to
    // leave alone.
    assert!(
        TRAP_STACK_BYTES == 112,
        "TaskContext changed size: update the -112/+112 literals in vectors.S"
    );
    // Xtensa requires a 16-byte-aligned stack pointer. An unaligned frame makes
    // every later `entry` misalign too, and the fault surfaces somewhere else
    // entirely.
    assert!(TRAP_STACK_BYTES % 16 == 0);
};

// No assembly prototypes are declared here. The trap entry in vectors.S reaches
// `_flint_trap` by symbol and the trampoline in context.S is reached by a
// restored PC, so nothing on the Rust side needs to name them.
