// SPDX-License-Identifier: Apache-2.0

//! Which Xtensa core is executing.

use hal::smp::{CoreId, MultiCore};

/// The ESP32's two LX6 cores.
pub struct XtensaSmp;

impl MultiCore for XtensaSmp {
    /// Read `PRID` and take bit 13.
    ///
    /// The full `PRID` values are 0xCDCD on PRO and 0xABAB on APP, and it is
    /// tempting to compare against those. esp-idf does not: `cpu_ll_get_core_id`
    /// is `rsr.prid` followed by `extui %0,%0,13,1`, because bit 13 is the part
    /// that actually distinguishes them and the rest is not a documented
    /// identity. Two instructions, no branch, which is what lets this sit at
    /// the top of a lock acquisition.
    #[inline(always)]
    fn current_core() -> CoreId {
        let id: u32;
        unsafe {
            core::arch::asm!(
                "rsr.prid {0}",
                "extui {0}, {0}, 13, 1",
                out(reg) id,
                options(nomem, nostack, preserves_flags)
            );
        }
        CoreId(id as u8)
    }

    fn cores() -> u8 {
        2
    }
}
