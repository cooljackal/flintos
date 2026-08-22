// SPDX-License-Identifier: Apache-2.0

use core::sync::atomic::{AtomicU32, Ordering};
use hal::tick::TickSource;

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
    /// Enable SysTick after the first SVC has become the active handler.
    ///
    /// # Safety
    /// The reload value must already have been configured by [`TickSource::init`].
    #[cfg(target_arch = "arm")]
    pub unsafe fn start() {
        unsafe { SYST_CSR.write_volatile(0b111) };
    }

    /// Stop SysTick without changing its reload value.
    ///
    /// # Safety
    /// The caller must restart it promptly; scheduler time is frozen meanwhile.
    #[cfg(target_arch = "arm")]
    pub unsafe fn stop() {
        unsafe { SYST_CSR.write_volatile(0) };
    }

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
            // PRIMASK is still set during Reset. Enabling the counter here can
            // make SysTick pending before the first task frame exists; SVC
            // starts it only after becoming the active handler.
            SYST_CSR.write_volatile(0);
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
