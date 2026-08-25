// SPDX-License-Identifier: Apache-2.0

#![no_std]

use soc_rp2040::{
    ctrl::{self, GpioPort},
    unreset, IO_BANK0_BASE, PADS_BANK0_BASE, PWM_BASE, RESET_IO_BANK0, RESET_PADS_BANK0, RESET_PWM,
    XOSC_HZ,
};

const SLICE_STRIDE: u32 = 0x14;
const CSR: u32 = 0x00;
const DIV: u32 = 0x04;
const CTR: u32 = 0x08;
const CC: u32 = 0x0c;
const TOP: u32 = 0x10;
const CSR_ENABLE: u32 = 1;
const DIV_ONE: u32 = 1 << 4;

#[cfg(target_arch = "arm")]
static mut CLAIMED_SLICES: u8 = 0;
#[cfg(not(target_arch = "arm"))]
static CLAIMED_SLICES: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

fn claim_slice(slice: u8) -> bool {
    let mask = 1 << slice;
    #[cfg(target_arch = "arm")]
    unsafe {
        const CLAIM_LOCK: *mut u32 = (soc_rp2040::SIO_BASE + 0x100 + 31 * 4) as *mut u32;
        while CLAIM_LOCK.read_volatile() == 0 {
            core::hint::spin_loop();
        }
        let claims = core::ptr::addr_of_mut!(CLAIMED_SLICES);
        let value = claims.read_volatile();
        let free = value & mask == 0;
        claims.write_volatile(value | mask);
        CLAIM_LOCK.write_volatile(1);
        free
    }
    #[cfg(not(target_arch = "arm"))]
    {
        CLAIMED_SLICES.fetch_or(mask, core::sync::atomic::Ordering::AcqRel) & mask == 0
    }
}

fn release_slice(slice: u8) {
    let mask = !(1 << slice);
    #[cfg(target_arch = "arm")]
    unsafe {
        const CLAIM_LOCK: *mut u32 = (soc_rp2040::SIO_BASE + 0x100 + 31 * 4) as *mut u32;
        while CLAIM_LOCK.read_volatile() == 0 {
            core::hint::spin_loop();
        }
        let claims = core::ptr::addr_of_mut!(CLAIMED_SLICES);
        claims.write_volatile(claims.read_volatile() & mask);
        CLAIM_LOCK.write_volatile(1);
    }
    #[cfg(not(target_arch = "arm"))]
    CLAIMED_SLICES.fetch_and(mask, core::sync::atomic::Ordering::Release);
}

fn channel_for_pin(pin: u8) -> hal::Result<(u8, u8)> {
    if pin >= 30 {
        return Err(hal::Error::Unsupported);
    }
    Ok(((pin >> 1) & 7, pin & 1))
}

fn timing(frequency_hz: u32, duty_per_mille: u16) -> hal::Result<(u32, u32)> {
    if frequency_hz == 0 || duty_per_mille > 1_000 {
        return Err(hal::Error::Unsupported);
    }
    let period = XOSC_HZ / frequency_hz;
    if !(2..=65_536).contains(&period) || XOSC_HZ % frequency_hz != 0 {
        return Err(hal::Error::Unsupported);
    }
    Ok((period - 1, period * u32::from(duty_per_mille) / 1_000))
}

pub struct Rp2040Pwm {
    pin: u8,
    slice: u8,
    channel: u8,
}

impl Rp2040Pwm {
    pub fn open(port: &GpioPort) -> hal::Result<Self> {
        let (slice, channel) = channel_for_pin(port.pin)?;
        if !ctrl::claim_gpio(port.pin) {
            return Err(hal::Error::Other("RP2040 PWM pin already in use"));
        }
        if !claim_slice(slice) {
            ctrl::release_gpio(port.pin);
            return Err(hal::Error::Other("RP2040 PWM slice already in use"));
        }
        Ok(Self {
            pin: port.pin,
            slice,
            channel,
        })
    }

    pub fn start(&self, frequency_hz: u32, duty_per_mille: u16) -> hal::Result<()> {
        let (top, compare) = timing(frequency_hz, duty_per_mille)?;
        let base = PWM_BASE + u32::from(self.slice) * SLICE_STRIDE;
        unsafe {
            unreset(RESET_PWM | RESET_IO_BANK0 | RESET_PADS_BANK0);
            let pad = (PADS_BANK0_BASE + 4 + u32::from(self.pin) * 4) as *mut u32;
            pad.write_volatile(
                (pad.read_volatile() & !((1 << 7) | (1 << 3) | (1 << 2))) | (1 << 6),
            );
            ((IO_BANK0_BASE + 4 + u32::from(self.pin) * 8) as *mut u32).write_volatile(4);
            ((base + CSR) as *mut u32).write_volatile(0);
            ((base + CTR) as *mut u32).write_volatile(0);
            let cc = (base + CC) as *mut u32;
            let shift = u32::from(self.channel) * 16;
            cc.write_volatile((cc.read_volatile() & !(0xffff << shift)) | (compare << shift));
            ((base + TOP) as *mut u32).write_volatile(top);
            ((base + DIV) as *mut u32).write_volatile(DIV_ONE);
            ((base + CSR) as *mut u32).write_volatile(CSR_ENABLE);
        }
        Ok(())
    }

    pub fn stop(&self) {
        let base = PWM_BASE + u32::from(self.slice) * SLICE_STRIDE;
        unsafe { ((base + CSR) as *mut u32).write_volatile(0) };
    }
}

impl Drop for Rp2040Pwm {
    fn drop(&mut self) {
        self.stop();
        release_slice(self.slice);
        ctrl::release_gpio(self.pin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpio_to_slice_and_channel_matches_the_datasheet() {
        assert_eq!(channel_for_pin(0), Ok((0, 0)));
        assert_eq!(channel_for_pin(1), Ok((0, 1)));
        assert_eq!(channel_for_pin(2), Ok((1, 0)));
        assert_eq!(channel_for_pin(17), Ok((0, 1)));
        assert!(channel_for_pin(30).is_err());
    }

    #[test]
    fn register_layout_matches_the_pwm_slice_block() {
        assert_eq!(
            (CSR, DIV, CTR, CC, TOP, SLICE_STRIDE),
            (0, 4, 8, 12, 16, 20)
        );
        assert_eq!(PWM_BASE, 0x4005_0000);
        assert_eq!(DIV_ONE, 0x10);
    }

    #[test]
    fn one_kilohertz_half_duty_uses_exact_integer_counts() {
        assert_eq!(timing(1_000, 500), Ok((11_999, 6_000)));
        assert!(timing(0, 500).is_err());
        assert!(timing(1_000, 1_001).is_err());
    }
}
