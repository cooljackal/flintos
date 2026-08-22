// SPDX-License-Identifier: Apache-2.0

use hal::tick::TickSource;
use core::sync::atomic::{AtomicU32, Ordering};

#[cfg(target_arch = "arm")]
const SYST_CSR: *mut u32 = 0xe000_e010 as *mut u32;
#[cfg(target_arch = "arm")]
const SYST_RVR: *mut u32 = 0xe000_e014 as *mut u32;
#[cfg(target_arch = "arm")]
const SYST_CVR: *mut u32 = 0xe000_e018 as *mut u32;

static mut NOW: u64 = 0;
static CPU_HZ: AtomicU32 = AtomicU32::new(0);
static PERIOD_US: AtomicU32 = AtomicU32::new(0);

pub struct Armv6mTick;

impl Armv6mTick {
    pub fn cpu_hz() -> u32 {
        CPU_HZ.load(Ordering::Relaxed)
    }

    pub fn ticks_per_period() -> u32 {
        ((Self::cpu_hz() as u64 * PERIOD_US.load(Ordering::Relaxed) as u64) / 1_000_000) as u32
    }
}

impl TickSource for Armv6mTick {
    fn init(period_us: u32, cpu_hz: u32) {
        let _reload = ((cpu_hz as u64 * period_us as u64) / 1_000_000)
            .saturating_sub(1)
            .min(0x00ff_ffff) as u32;
        CPU_HZ.store(cpu_hz, Ordering::Relaxed);
        PERIOD_US.store(period_us, Ordering::Relaxed);
        #[cfg(target_arch = "arm")]
        unsafe {
            SYST_RVR.write_volatile(_reload);
            SYST_CVR.write_volatile(0);
            SYST_CSR.write_volatile(0b111);
        }
    }

    fn tick() -> bool {
        crate::cs_with(|| unsafe { NOW = NOW.wrapping_add(1) });
        false
    }

    fn now() -> u64 {
        crate::cs_with(|| unsafe { NOW })
    }
}
