// SPDX-License-Identifier: Apache-2.0

//! MPU (Memory Protection Unit) manager trait.
//!
//! Legacy hooks, not the active isolated-task contract. The kernel does not
//! call these hooks at boot or on context switches; the existing Xtensa
//! implementation is a no-op. See `crate::isolation` for validated task grants.

use crate::types::*;

// The opt-in isolated-task path uses `crate::isolation` and a fallible geometry
// contract. These legacy infallible hooks do not establish an isolation domain.

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
