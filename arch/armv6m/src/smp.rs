// SPDX-License-Identifier: Apache-2.0

use hal::smp::{CoreId, MultiCore};

pub struct Armv6mSmp;

impl MultiCore for Armv6mSmp {
    fn current_core() -> CoreId {
        #[cfg(target_arch = "arm")]
        return CoreId(unsafe { crate::SIO_CPUID.read_volatile() as u8 });
        #[cfg(not(target_arch = "arm"))]
        return CoreId(0);
    }

    fn cores() -> u8 {
        2
    }

    fn request_reschedule(core: CoreId) -> bool {
        #[cfg(target_arch = "arm")]
        unsafe {
            unsafe extern "C" {
                fn _flint_armv6m_request_reschedule(core: u32) -> bool;
            }
            return _flint_armv6m_request_reschedule(u32::from(core.0));
        }
        #[cfg(not(target_arch = "arm"))]
        {
            let _ = core;
            false
        }
    }

    fn context_id() -> u8 {
        Self::current_core().0
    }
}
