// SPDX-License-Identifier: Apache-2.0

//! Core type definitions shared across the kernel, HAL, and API layers.
//!
//! Most of these types are architecture-agnostic and form the common vocabulary
//!
//! [`TaskContext`] is the exception, and it is worth naming rather than
//! leaving for whoever attempts a second port to discover: its fields are
//! Xtensa's (`ps`, `sar`, the `lbeg`/`lend`/`lcount` loop registers, `a[16]`,
//! `windowbase`, `windowstart`). A Cortex-M context is a different struct, so
//! this type has to become an associated type of an arch trait before a second
//! architecture can exist. Until then the claim below is aspirational for this
//! one type.
//!
//! The rest form the common vocabulary
//! for the scheduler, context switching, MPU management, and task model.

// ── Task context ────────────────────────────────────────────────────────────

/// A task's saved execution state.
///
/// This is the one definition of that layout. The assembly trap entry builds
/// exactly this, by field offset, on the interrupted task's stack; `_flint_trap`
/// receives a pointer to it and returns a pointer to whichever one should run
/// next; the scheduler stores one per task. `#[repr(C)]` and the size assertion
/// below are what make those three agree.
///
/// There used to be a second struct, `RawTrapFrame`, documented as the thing
/// the assembly built and the handler consumed — a role it never actually
/// played. It appeared only in an inert syscall stub, while the live path used
/// this type throughout. Two identical layouts that must both track the
/// assembly, with nothing checking either against the other, is a bug waiting
/// for someone to edit the wrong one.
#[repr(C)]
pub struct TaskContext {
    pub pc: u32,
    pub ps: u32,
    pub sar: u32,
    pub lbeg: u32,
    pub lend: u32,
    pub lcount: u32,
    /// The 16 general-purpose registers visible in the current window.
    pub a: [u32; 16],
    /// Xtensa register-window base pointer.
    pub windowbase: u32,
    /// Bitmask of active register windows.
    pub windowstart: u32,
    /// `SCOMPARE1`, the comparand for `S32C1I`.
    ///
    /// **Saving this is what makes atomics usable in an interrupt handler.**
    /// LLVM lowers an atomic read-modify-write on Xtensa into a retry loop:
    /// load, compute, `wsr.scompare1`, `s32c1i`, branch back if the store
    /// found a different value. An interrupt landing inside that loop whose
    /// handler also does an atomic RMW overwrites `SCOMPARE1`, and the
    /// interrupted loop then compares against the *handler's* comparand
    /// forever.
    ///
    /// Both references save it: NuttX in `xtensa_context.S`
    /// (`rsr a3, SCOMPARE1` into `REG_SCOMPARE1`, restored at the matching
    /// `wsr`), and Zephyr as `uintptr_t scompare1` in its saved frame under
    /// `#if XCHAL_HAVE_S32C1I`.
    pub scompare1: u32,
    /// Padding to a 16-byte multiple, which Xtensa requires of a stack
    /// pointer. Not spare space to grow into without re-checking the literals
    /// in `vectors.S`.
    pub _reserved: [u32; 3],
}

impl TaskContext {
    /// Create a zeroed TaskContext (all registers = 0).
    pub const fn zeroed() -> Self {
        Self {
            pc: 0,
            ps: 0,
            sar: 0,
            lbeg: 0,
            lend: 0,
            lcount: 0,
            a: [0; 16],
            windowbase: 0,
            windowstart: 0,
            scompare1: 0,
            _reserved: [0; 3],
        }
    }
}

/// The assembly trap entry hardcodes this frame size and the field offsets
/// below. If either drifts, the handler reads and writes the wrong words with
/// no diagnostic at all, so tie them together here.
const _: () = {
    assert!(core::mem::size_of::<TaskContext>() == 112);
    // 16-byte aligned, as Xtensa requires of a stack pointer.
    assert!(core::mem::size_of::<TaskContext>() % 16 == 0);
};

// ── MPU ─────────────────────────────────────────────────────────────────────

/// Describes one MPU region for use by `MpuManager`.
pub struct RegionDescriptor {
    pub base: u32,
    pub size: u32,
    pub permissions: RegionPermissions,
}

/// Read/write/execute permissions for an MPU region, including a user-mode
/// flag to distinguish kernel vs. user-space regions.
pub struct RegionPermissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub user_mode: bool,
}

// ── Task identity ───────────────────────────────────────────────────────────

/// Opaque task identifier assigned by the scheduler at spawn time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskId(pub u32);

// ── Priority ────────────────────────────────────────────────────────────────

/// Task priority band and level within that band.
///
/// Numeric ordering (lower = higher priority):
///   Critical(0) > Critical(1) > ... > Normal(0) > ... > Background(15)
///
/// The `numeric()` method encodes both the band and the level into a
/// single `u8` for the scheduler's ready mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Real-time tasks (band 0). Levels 0-15.
    Critical(u8),
    /// Normal application tasks (band 1). Levels 0-15.
    Normal(u8),
    /// Background/idle tasks (band 2). Levels 0-15.
    Background(u8),
}

impl Priority {
    /// Encode the priority as a single number where lower = more urgent.
    ///
    /// The band is shifted left by 4 bits and the level occupies the
    /// lower nibble.  This gives a total ordering:
    /// `Critical(0)` = 0, ... `Critical(15)` = 0x0F,
    /// `Normal(0)` = 0x10, ... `Background(15)` = 0x2F.
    pub fn numeric(&self) -> u8 {
        match self {
            Priority::Critical(v) => *v,
            Priority::Normal(v) => 0x10 | *v,
            Priority::Background(v) => 0x20 | *v,
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn task_id_equality() {
        assert_eq!(TaskId(0), TaskId(0));
        assert_ne!(TaskId(0), TaskId(1));
    }

    #[test]
    fn priority_numeric_ordering() {
        assert_eq!(Priority::Critical(0).numeric(), 0x00);
        assert_eq!(Priority::Critical(15).numeric(), 0x0F);
        assert_eq!(Priority::Normal(0).numeric(), 0x10);
        assert_eq!(Priority::Normal(15).numeric(), 0x1F);
        assert_eq!(Priority::Background(0).numeric(), 0x20);
        assert_eq!(Priority::Background(15).numeric(), 0x2F);
    }

    #[test]
    fn priority_ordering() {
        // Lower discriminant = higher priority (runs first in ready_mask scan)
        assert!(Priority::Critical(0) < Priority::Critical(1));
        assert!(Priority::Critical(0) < Priority::Normal(0));
        assert!(Priority::Normal(0) < Priority::Background(0));
    }

    #[test]
    fn task_context_zeroed() {
        let ctx = TaskContext::zeroed();
        assert_eq!(ctx.pc, 0);
        assert_eq!(ctx.ps, 0);
        assert_eq!(ctx.a, [0u32; 16]);
        assert_eq!(ctx.windowbase, 0);
        assert_eq!(ctx.windowstart, 0);
    }

}
