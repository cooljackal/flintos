// SPDX-License-Identifier: Apache-2.0

//! MPU (Memory Protection Unit) manager trait.
//!
//! Contract for future task isolation using hardware protection regions.
//! The kernel does not yet call these hooks at boot or on context switches;
//! the existing Xtensa implementation is a no-op. This trait alone provides
//! no isolation. Integration and supported-processor enforcement are #139.

use crate::types::*;

/// Manages the hardware MPU regions for task isolation.
pub trait MpuManager {
    /// Configure one MPU region at the given index.
    fn configure_region(index: u8, desc: &RegionDescriptor);

    /// Activate the set of regions belonging to a task.
    fn activate_task_regions(task: &TaskDescriptor);

    /// Clear all user-mode regions (enter kernel mode).
    fn clear_user_regions();
}

/// Scheduler-facing task descriptor read by the MPU manager.
pub struct TaskDescriptor {
    pub id: TaskId,
    pub stack_base: u32,
    pub stack_size: u32,
    pub data_regions: &'static [RegionDescriptor],
}
