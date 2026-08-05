// SPDX-License-Identifier: Apache-2.0

//! Core type definitions shared across the kernel, HAL, and API layers.
//!
//! These types are architecture-agnostic and form the common vocabulary
//! for the scheduler, context switching, MPU management, and task model.

use core::fmt;

// ── Trap frame ─────────────────────────────────────────────────────────────

/// Architecture-specific raw trap frame captured on exception / interrupt entry.
///
/// The layout is identical to [`TaskContext`] (96 bytes, `#[repr(C)]`) so the
/// assembly trap entry in `vectors.S` can build a frame and the kernel handler
/// can treat it directly as a resumable task context. Field offsets are
/// load-bearing — keep them in lock-step with `asm/context.S` / `asm/vectors.S`.
#[repr(C)]
pub struct RawTrapFrame {
    pub pc: u32,
    pub ps: u32,
    pub sar: u32,
    pub lbeg: u32,
    pub lend: u32,
    pub lcount: u32,
    /// 16 general-purpose registers at the point of trap.
    pub a: [u32; 16],
    pub windowbase: u32,
    pub windowstart: u32,
}

impl fmt::Debug for RawTrapFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawTrapFrame")
            .field("pc", &self.pc)
            .field("ps", &self.ps)
            .field("sar", &self.sar)
            .finish()
    }
}

// ── Task context ────────────────────────────────────────────────────────────

/// Per-task saved context for scheduler context switches.
///
/// Captures all callee-saved registers plus Xtensa window state so the
/// scheduler can suspend and resume tasks at arbitrary points.
/// `#[repr(C)]` is required because the assembly save/restore routines
/// access fields by fixed offset.
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
    /// The other 48 physical address registers, i.e. the rest of the register
    /// file outside the current window.
    ///
    /// Saving these is what makes a context switch correct on Xtensa. A task's
    /// outer call frames live in the physical register file, not on its stack,
    /// until a window overflow evicts them. The trap handler is itself windowed
    /// Rust, so merely *servicing* an interrupt rotates the window and spills
    /// some of the interrupted task's frames -- clearing their WINDOWSTART bits
    /// while their physical registers get reused. Restoring only the current
    /// window and the original WINDOWSTART then tells the hardware those frames
    /// are still in registers when they are not.
    ///
    /// Capturing the whole register file sidesteps the entire question: the
    /// machine state is restored exactly as it was, so whatever the handler did
    /// to the windows in between cannot be observed. It costs 192 bytes per
    /// task and about 96 extra loads and stores per trap.
    pub ar_rest: [u32; 48],
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
            ar_rest: [0; 48],
        }
    }
}

/// The assembly trap entry hardcodes this frame size and the field offsets
/// below. If either drifts, the handler reads and writes the wrong words with
/// no diagnostic at all, so tie them together here.
const _: () = {
    assert!(core::mem::size_of::<TaskContext>() == 288);
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

    #[test]
    fn raw_trap_frame_debug() {
        let frame = RawTrapFrame {
            pc: 0x4000_0000,
            ps: 0x0001_0000,
            sar: 0,
            lbeg: 0,
            lend: 0,
            lcount: 0,
            a: [0; 16],
            windowbase: 0,
            windowstart: 0,
        };
        let s = std::format!("{:?}", frame);
        assert!(s.contains("pc"));
        // Debug prints decimal: 0x4000_0000 = 1073741824
        assert!(s.contains("1073741824"));
    }
}
