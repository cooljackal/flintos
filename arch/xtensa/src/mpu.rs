// SPDX-License-Identifier: Apache-2.0

//! MPU manager — OPTIONAL (may never ship).
//!
//! Per the locked architecture decision, FlintOS is a **single protection
//! domain**: there is no hardware memory isolation between tasks, and the
//! kernel's correctness does not depend on any. This type exists only as an
//! inert seam so the option of adding ESP32 PMS/PID-based protection later
//! remains open. Every method is a no-op; nothing on the hot path calls it.
//!
//! If isolation is ever implemented, it would program the ESP32 Permission
//! Control (PMS) registers here — note the ESP32 "MPU" is a fixed-region
//! permission controller, *not* an ARM-style base/size region MPU, so the old
//! `base | permission_bits` encoding was meaningless and has been removed.

use hal::mpu::{MpuManager, TaskDescriptor};
use hal::types::RegionDescriptor;

/// No-op MPU manager for the single-protection-domain configuration.
pub struct Esp32Mpu;

impl MpuManager for Esp32Mpu {
    fn configure_region(_index: u8, _desc: &RegionDescriptor) {
        // DEFERRED / OPTIONAL: no hardware isolation in this configuration.
    }

    fn activate_task_regions(_task: &TaskDescriptor) {
        // DEFERRED / OPTIONAL: no per-task regions to activate.
    }

    fn clear_user_regions() {
        // DEFERRED / OPTIONAL: nothing to clear.
    }
}
