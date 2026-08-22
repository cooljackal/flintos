// SPDX-License-Identifier: Apache-2.0

use hal::smp::{CoreId, MultiCore};

pub struct Armv6mSmp;

impl MultiCore for Armv6mSmp {
    fn current_core() -> CoreId {
        #[cfg(target_arch = "arm")]
        return CoreId(unsafe { (0xd000_0000 as *const u32).read_volatile() as u8 });
        #[cfg(not(target_arch = "arm"))]
        return CoreId(0);
    }

    fn cores() -> u8 {
        2
    }

    fn context_id() -> u8 {
        Self::current_core().0
    }
}
