// SPDX-License-Identifier: Apache-2.0

//! RP2040's single 12-bit SAR ADC.
//!
//! GPIO 26 through 29 are channels 0 through 3. Channel 4 is the internal
//! temperature sensor. One [`Rp2040Adc`] owns the shared converter and claims
//! external GPIOs as they are first used.

#![no_std]

use soc_rp2040::{
    ctrl, unreset, ADC_BASE, IO_BANK0_BASE, PADS_BANK0_BASE, RESET_ADC, RESET_IO_BANK0,
    RESET_PADS_BANK0,
};

const CS: u32 = ADC_BASE;
const RESULT: u32 = ADC_BASE + 0x04;
const CS_EN: u32 = 1;
const CS_TS_EN: u32 = 1 << 1;
const CS_START_ONCE: u32 = 1 << 2;
const CS_READY: u32 = 1 << 8;
const CS_ERR: u32 = 1 << 9;
const CS_AINSEL_SHIFT: u32 = 12;
const RESULT_MASK: u32 = 0x0fff;
const CONVERSION_TIMEOUT_US: u32 = 1_000;

#[cfg(target_arch = "arm")]
fn now_us() -> u32 {
    soc_rp2040::timer_us()
}

#[cfg(not(target_arch = "arm"))]
fn now_us() -> u32 {
    static CLOCK: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
    CLOCK.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
}

#[cfg(target_arch = "arm")]
static mut ADC_CLAIMED: bool = false;
#[cfg(not(target_arch = "arm"))]
static ADC_CLAIMED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn claim_adc() -> bool {
    #[cfg(target_arch = "arm")]
    unsafe {
        const LOCK: *mut u32 = (soc_rp2040::SIO_BASE + 0x100 + 31 * 4) as *mut u32;
        while LOCK.read_volatile() == 0 {
            core::hint::spin_loop();
        }
        let claimed = core::ptr::addr_of_mut!(ADC_CLAIMED);
        let free = !claimed.read_volatile();
        claimed.write_volatile(true);
        LOCK.write_volatile(1);
        free
    }
    #[cfg(not(target_arch = "arm"))]
    {
        !ADC_CLAIMED.swap(true, core::sync::atomic::Ordering::AcqRel)
    }
}

fn release_adc() {
    #[cfg(target_arch = "arm")]
    unsafe {
        const LOCK: *mut u32 = (soc_rp2040::SIO_BASE + 0x100 + 31 * 4) as *mut u32;
        while LOCK.read_volatile() == 0 {
            core::hint::spin_loop();
        }
        core::ptr::addr_of_mut!(ADC_CLAIMED).write_volatile(false);
        LOCK.write_volatile(1);
    }
    #[cfg(not(target_arch = "arm"))]
    ADC_CLAIMED.store(false, core::sync::atomic::Ordering::Release);
}

/// One of the RP2040 ADC inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Gpio26 = 0,
    Gpio27 = 1,
    Gpio28 = 2,
    Gpio29 = 3,
    Temperature = 4,
}

impl Channel {
    pub const fn from_gpio(pin: u8) -> Option<Self> {
        Some(match pin {
            26 => Self::Gpio26,
            27 => Self::Gpio27,
            28 => Self::Gpio28,
            29 => Self::Gpio29,
            _ => return None,
        })
    }

    pub const fn gpio(self) -> Option<u8> {
        match self {
            Self::Gpio26 => Some(26),
            Self::Gpio27 => Some(27),
            Self::Gpio28 => Some(28),
            Self::Gpio29 => Some(29),
            Self::Temperature => None,
        }
    }
}

/// A conversion failure distinguishable from ownership errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdcError {
    Timeout,
    Conversion,
    PinInUse,
}

impl From<AdcError> for hal::Error {
    fn from(error: AdcError) -> Self {
        match error {
            AdcError::Timeout => Self::Other("RP2040 ADC conversion timed out"),
            AdcError::Conversion => Self::Other("RP2040 ADC conversion failed"),
            AdcError::PinInUse => Self::Other("RP2040 ADC pin already in use"),
        }
    }
}

/// Convert a raw count to microvolts using the nominal 3.3 V reference.
///
/// This is appropriate for the temperature formula and diagnostics. It is not
/// calibrated voltage metrology: the reference and ADC both vary by device.
pub const fn raw_to_microvolts(raw: u16) -> u32 {
    ((raw as u64 * 3_300_000 + 2_047) / 4_095) as u32
}

/// Convert the internal sensor's raw count to nominal milli-degrees Celsius.
///
/// Formula and typical constants are from the RP2040 datasheet. The sensor is
/// not factory calibrated, so callers must treat this as an approximate chip
/// temperature rather than an ambient-temperature measurement.
pub const fn temperature_milli_celsius(raw: u16) -> i32 {
    let delta_uv = raw_to_microvolts(raw) as i64 - 706_000;
    (27_000i64 - delta_uv * 1_000 / 1_721) as i32
}

/// Exclusive owner of the shared RP2040 ADC.
pub struct Rp2040Adc {
    claimed_gpio: u8,
}

impl Rp2040Adc {
    pub fn open() -> hal::Result<Self> {
        if !claim_adc() {
            return Err(hal::Error::Other("RP2040 ADC already in use"));
        }
        soc_rp2040::enable_adc_clock();
        unsafe {
            unreset(RESET_ADC);
            (CS as *mut u32).write_volatile(CS_EN);
        }
        Ok(Self { claimed_gpio: 0 })
    }

    fn prepare_gpio(&mut self, pin: u8) -> Result<(), AdcError> {
        let mask = 1u8 << (pin - 26);
        if self.claimed_gpio & mask != 0 {
            return Ok(());
        }
        if !ctrl::claim_gpio(pin) {
            return Err(AdcError::PinInUse);
        }
        unsafe {
            unreset(RESET_IO_BANK0 | RESET_PADS_BANK0);
            let pad = (PADS_BANK0_BASE + 4 + u32::from(pin) * 4) as *mut u32;
            pad.write_volatile(pad.read_volatile() & !((1 << 6) | (1 << 3) | (1 << 2)));
            // Function 31 disconnects the digital peripheral mux from the pad.
            ((IO_BANK0_BASE + 4 + u32::from(pin) * 8) as *mut u32).write_volatile(31);
        }
        self.claimed_gpio |= mask;
        Ok(())
    }

    /// Perform one bounded conversion and return the raw 12-bit count.
    pub fn read(&mut self, channel: Channel) -> Result<u16, AdcError> {
        if let Some(pin) = channel.gpio() {
            self.prepare_gpio(pin)?;
        }
        let config = CS_EN
            | ((channel as u32) << CS_AINSEL_SHIFT)
            | if channel == Channel::Temperature {
                CS_TS_EN
            } else {
                0
            };
        unsafe {
            (CS as *mut u32).write_volatile(config);
            ((CS + 0x2000) as *mut u32).write_volatile(CS_START_ONCE);
            let start = now_us();
            while (CS as *const u32).read_volatile() & CS_READY == 0 {
                if now_us().wrapping_sub(start) >= CONVERSION_TIMEOUT_US {
                    return Err(AdcError::Timeout);
                }
                core::hint::spin_loop();
            }
            if (CS as *const u32).read_volatile() & CS_ERR != 0 {
                return Err(AdcError::Conversion);
            }
            Ok(((RESULT as *const u32).read_volatile() & RESULT_MASK) as u16)
        }
    }

    pub fn read_averaged(&mut self, channel: Channel, samples: u16) -> Result<u16, AdcError> {
        if samples == 0 {
            return self.read(channel);
        }
        let mut total = 0u32;
        for _ in 0..samples {
            total += u32::from(self.read(channel)?);
        }
        Ok((total / u32::from(samples)) as u16)
    }
}

impl Drop for Rp2040Adc {
    fn drop(&mut self) {
        unsafe { (CS as *mut u32).write_volatile(0) };
        for offset in 0..4u8 {
            if self.claimed_gpio & (1 << offset) != 0 {
                ctrl::release_gpio(26 + offset);
            }
        }
        release_adc();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_layout_matches_the_generated_sdk_header() {
        assert_eq!(ADC_BASE, 0x4004_c000);
        assert_eq!((CS, RESULT), (ADC_BASE, ADC_BASE + 4));
        assert_eq!((CS_READY, CS_ERR, CS_AINSEL_SHIFT), (0x100, 0x200, 12));
    }

    #[test]
    fn gpio_channels_are_exactly_26_through_29() {
        for pin in 26..=29 {
            assert_eq!(Channel::from_gpio(pin).and_then(Channel::gpio), Some(pin));
        }
        assert_eq!(Channel::from_gpio(25), None);
        assert_eq!(Channel::from_gpio(30), None);
        assert_eq!(Channel::Temperature.gpio(), None);
    }

    #[test]
    fn datasheet_typical_voltage_is_about_27_celsius() {
        let raw = ((706_000u64 * 4_095 + 1_650_000) / 3_300_000) as u16;
        assert!((temperature_milli_celsius(raw) - 27_000).abs() < 500);
        assert_eq!(raw_to_microvolts(0), 0);
        assert_eq!(raw_to_microvolts(4_095), 3_300_000);
    }
}
