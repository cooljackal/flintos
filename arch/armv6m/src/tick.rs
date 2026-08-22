// SPDX-License-Identifier: Apache-2.0

use core::sync::atomic::{AtomicU32, Ordering};
use hal::tick::TickSource;

#[cfg(target_arch = "arm")]
const SYST_CSR: *mut u32 = 0xe000_e010 as *mut u32;
#[cfg(target_arch = "arm")]
const SYST_RVR: *mut u32 = 0xe000_e014 as *mut u32;
#[cfg(target_arch = "arm")]
const SYST_CVR: *mut u32 = 0xe000_e018 as *mut u32;
/// SysTick's reload register is 24-bit; a larger value silently truncates.
#[cfg(target_arch = "arm")]
const SYST_RELOAD_MAX: u32 = 0x00ff_ffff;

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
        let reload = Self::reload_value();
        unsafe {
            // SysTick is banked per Cortex-M0+ core. Core 1 therefore needs
            // its own reload/current setup even though core 0 published the
            // shared frequency and period during global initialization.
            SYST_RVR.write_volatile(reload);
            SYST_CVR.write_volatile(0);
            SYST_CSR.write_volatile(0b111);
        }
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

    /// The SysTick reload value for the configured frequency and period: one
    /// less than the per-period tick count (the counter reloads the cycle after
    /// it reaches zero), clamped to the 24-bit reload register.
    #[cfg(target_arch = "arm")]
    fn reload_value() -> u32 {
        Self::ticks_per_period().saturating_sub(1).min(SYST_RELOAD_MAX)
    }

    /// Whether this core owns the system-wide monotonic clock.
    pub fn is_timekeeper_core() -> bool {
        #[cfg(target_arch = "arm")]
        return unsafe { crate::SIO_CPUID.read_volatile() == 0 };
        #[cfg(not(target_arch = "arm"))]
        true
    }
}

impl TickSource for Armv6mTick {
    fn init(period_us: u32, cpu_hz: u32) {
        CPU_HZ.store(cpu_hz, Ordering::Relaxed);
        PERIOD_US.store(period_us, Ordering::Relaxed);
        #[cfg(target_arch = "arm")]
        unsafe {
            SYST_RVR.write_volatile(Self::reload_value());
            SYST_CVR.write_volatile(0);
            // PRIMASK is still set during Reset. Enabling the counter here can
            // make SysTick pending before the first task frame exists; SVC
            // starts it only after becoming the active handler.
            SYST_CSR.write_volatile(0);
        }
    }

    fn tick() -> bool {
        if Self::is_timekeeper_core() {
            crate::cs_with(|| unsafe { NOW = NOW.wrapping_add(1) });
        }
        false
    }

    fn now() -> u64 {
        crate::cs_with(|| unsafe { NOW })
    }
}
