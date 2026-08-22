// SPDX-License-Identifier: Apache-2.0

//! Core type definitions shared across the kernel, HAL, and API layers.
//!
//! These types are architecture-agnostic vocabulary for MPU management and
//! the task model. Saved register frames live in architecture crates.

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
}
