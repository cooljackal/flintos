//! MPU (Memory Protection Unit) manager trait.
//!
//! Provides task isolation by configuring hardware memory-protection
//! regions.  The kernel calls `configure_region` at boot and
//! `activate_task_regions` / `clear_user_regions` on context switches.

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