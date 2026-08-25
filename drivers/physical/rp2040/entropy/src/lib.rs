// SPDX-License-Identifier: Apache-2.0

//! A conditioned, best-effort RP2040 entropy seed.
//!
//! The RP2040 has no cryptographic random-number generator. Its ring
//! oscillator exposes one jitter-derived bit, which this driver samples no
//! faster than once every 10 microseconds and conditions with SplitMix64. This
//! follows the source and timing policy used by the Raspberry Pi Pico SDK, but
//! deliberately does **not** claim cryptographic strength. Use the result to
//! diversify non-secret PRNG state; do not generate keys from it.

#![no_std]

#[cfg(any(target_arch = "arm", test))]
use soc_rp2040::ROSC_BASE;

#[cfg(any(target_arch = "arm", test))]
const CLOCKS_CLK_SYS_CTRL: u32 = 0x4000_803c;
#[cfg(any(target_arch = "arm", test))]
const ROSC_STATUS: u32 = ROSC_BASE + 0x18;
#[cfg(any(target_arch = "arm", test))]
const ROSC_RANDOMBIT: u32 = ROSC_BASE + 0x1c;
#[cfg(any(target_arch = "arm", test))]
const ROSC_ENABLED: u32 = 1 << 12;
#[cfg(any(target_arch = "arm", test))]
const CLK_SYS_SRC_AUX: u32 = 1;
#[cfg(any(target_arch = "arm", test))]
const CLK_SYS_AUXSRC_SHIFT: u32 = 5;
#[cfg(any(target_arch = "arm", test))]
const CLK_SYS_AUXSRC_MASK: u32 = 0x7;
#[cfg(any(target_arch = "arm", test))]
const CLK_SYS_AUXSRC_ROSC: u32 = 0;
#[cfg(any(target_arch = "arm", test))]
const SAMPLE_INTERVAL_US: u32 = 10;

#[cfg(target_arch = "arm")]
fn now_us() -> u32 {
    soc_rp2040::timer_us()
}

/// Health evidence about the raw ROSC bits before conditioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntropyHealth {
    pub bits: u16,
    pub ones: u16,
    pub transitions: u16,
}

/// A conditioned seed whose quality is explicitly best effort, not crypto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BestEffortSeed {
    pub words: [u64; 2],
    pub health: EntropyHealth,
}

/// Why the raw source could not safely be sampled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntropyError {
    OscillatorDisabled,
    SystemClockUsesOscillator,
}

impl From<EntropyError> for hal::Error {
    fn from(error: EntropyError) -> Self {
        match error {
            EntropyError::OscillatorDisabled => Self::Other("RP2040 ROSC is disabled"),
            EntropyError::SystemClockUsesOscillator => {
                Self::Other("RP2040 system clock uses the entropy oscillator")
            }
        }
    }
}

#[cfg(any(target_arch = "arm", test))]
const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(any(target_arch = "arm", test))]
fn condition(raw: u64, time: u64, health: EntropyHealth) -> BestEffortSeed {
    let evidence = (u64::from(health.ones) << 48)
        | (u64::from(health.transitions) << 32)
        | u64::from(health.bits);
    let first = splitmix64(raw ^ time.rotate_left(17) ^ evidence);
    let second = splitmix64(raw.rotate_left(29) ^ time ^ first);
    BestEffortSeed {
        words: [first, second],
        health,
    }
}

#[cfg(any(target_arch = "arm", test))]
fn system_clock_uses_rosc(ctrl: u32) -> bool {
    ctrl & CLK_SYS_SRC_AUX != 0
        && (ctrl >> CLK_SYS_AUXSRC_SHIFT) & CLK_SYS_AUXSRC_MASK == CLK_SYS_AUXSRC_ROSC
}

/// Capture 64 spaced raw bits and return a conditioned best-effort seed.
#[cfg(target_arch = "arm")]
pub fn sample_seed() -> Result<BestEffortSeed, EntropyError> {
    unsafe {
        if (ROSC_STATUS as *const u32).read_volatile() & ROSC_ENABLED == 0 {
            return Err(EntropyError::OscillatorDisabled);
        }
        if system_clock_uses_rosc((CLOCKS_CLK_SYS_CTRL as *const u32).read_volatile()) {
            return Err(EntropyError::SystemClockUsesOscillator);
        }

        // Serialize the sample schedule across cores. Flint's DMA bookkeeping
        // uses spinlock 30 and peripheral ownership uses 31, so this source
        // owns 29 and cannot stall either subsystem during the spaced sample.
        const LOCK: *mut u32 = (soc_rp2040::SIO_BASE + 0x100 + 29 * 4) as *mut u32;
        while LOCK.read_volatile() == 0 {
            core::hint::spin_loop();
        }
        let started = soc_rp2040::timer_us_64();
        let mut deadline = now_us();
        let mut raw = 0u64;
        let mut ones = 0u16;
        let mut transitions = 0u16;
        let mut previous = 0u32;
        for index in 0..64 {
            while now_us().wrapping_sub(deadline) < SAMPLE_INTERVAL_US {
                core::hint::spin_loop();
            }
            deadline = now_us();
            let bit = (ROSC_RANDOMBIT as *const u32).read_volatile() & 1;
            raw = (raw << 1) | u64::from(bit);
            ones += bit as u16;
            if index != 0 && bit != previous {
                transitions += 1;
            }
            previous = bit;
        }
        LOCK.write_volatile(1);
        Ok(condition(
            raw,
            started,
            EntropyHealth {
                bits: 64,
                ones,
                transitions,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_facts_match_generated_pico_sdk_headers() {
        assert_eq!(ROSC_BASE, 0x4006_0000);
        assert_eq!(CLOCKS_CLK_SYS_CTRL, 0x4000_803c);
        assert_eq!((ROSC_STATUS, ROSC_RANDOMBIT), (0x4006_0018, 0x4006_001c));
        assert_eq!(ROSC_ENABLED, 0x1000);
        assert_eq!(SAMPLE_INTERVAL_US, 10);
    }

    #[test]
    fn the_rosc_must_not_drive_clk_sys() {
        assert!(!system_clock_uses_rosc(0));
        assert!(system_clock_uses_rosc(1));
        assert!(!system_clock_uses_rosc(1 | (1 << CLK_SYS_AUXSRC_SHIFT)));
    }

    #[test]
    fn conditioning_is_deterministic_and_uses_each_input() {
        let health = EntropyHealth {
            bits: 64,
            ones: 31,
            transitions: 29,
        };
        let seed = condition(0x0123_4567_89ab_cdef, 7, health);
        assert_eq!(seed, condition(0x0123_4567_89ab_cdef, 7, health));
        assert_ne!(seed, condition(0x0123_4567_89ab_cdee, 7, health));
        assert_ne!(seed, condition(0x0123_4567_89ab_cdef, 8, health));
        assert_ne!(seed.words[0], seed.words[1]);
    }
}
