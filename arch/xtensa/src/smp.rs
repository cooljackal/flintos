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

    /// Interrupt `core` through the kernel's cross-core reschedule channel.
    ///
    /// The mechanism needs the ESP32 DPORT `FromCpu` signals and the kernel's
    /// interrupt plumbing, neither of which this `hal`-only crate may name, so
    /// it is implemented in `kernel::xtensa` behind this hook — the same split
    /// the RP2040 uses for `_flint_armv6m_request_reschedule`.
    fn request_reschedule(core: CoreId) -> bool {
        extern "C" {
            fn _flint_xtensa_request_reschedule(core: u32) -> bool;
        }
        // SAFETY: a plain FFI call to a kernel-provided function that only reads
        // core state and raises a DPORT signal.
        unsafe { _flint_xtensa_request_reschedule(u32::from(core.0)) }
    }
}
