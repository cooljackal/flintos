// SPDX-License-Identifier: Apache-2.0

//! LEDC: the ESP32's PWM generator.
//!
//! Eight high-speed channels sharing four timers. A timer sets the period; a
//! channel picks a timer and a duty. Despite the name it is a general PWM
//! peripheral — servos, motor drivers and buzzers all use it.
//!
//! This drives the **high-speed** channels only. The low-speed ones exist
//! (`LEDC_LSCH0_CONF0_REG` at offset 0xA0) and differ in that they can run
//! from the slow clock while the CPU sleeps. Nothing here sleeps yet, and a
//! second set of channels with subtly different registers is a second set of
//! things to get wrong.
//!
//! # The one encoding that is not obvious
//!
//! `LEDC_HSCHn_DUTY_REG` does **not** hold the duty. It holds `duty << 4`:
//! bits `[24:4]` are the integer part and the low four bits are a fraction the
//! hardware uses to dither between two `lpoint` values. Writing a duty
//! directly gives one sixteenth of the intended output — a signal that looks
//! plausible on a scope and is wrong everywhere.
//!
//! Quoted from `ledc_reg.h`:
//!
//! > `reg_lpoint_hsch0 = (reg_hpoint_hsch0[19:0] + reg_duty_hsch0[24:4])`
//! > … The least four bits in this register represent the decimal part
//!
//! [`Channel::set_duty`] takes a plain duty and does the shift.
//!
//! # Register facts
//!
//! From `soc/ledc_reg.h`, `soc/soc.h`, `soc/dport_reg.h` and
//! `soc/gpio_sig_map.h`, read rather than recalled:
//!
//! | | |
//! |---|---|
//! | `DR_REG_LEDC_BASE` | `0x3ff59000` |
//! | HS channel stride | `0x14` — five registers each |
//! | HS timer stride | `0x08` |
//! | `LEDC_TICK_SEL_HSTIMERn` | bit 25, **1 = APB, 0 = REF_TICK** |
//! | `LEDC_HS_SIG_OUT0_IDX` | 71 |
//! | `DPORT_LEDC_CLK_EN` | `BIT(11)` |

#![no_std]

use soc_esp32::addr::LEDC_BASE;

/// APB clock, the only source this driver uses.
pub const APB_HZ: u32 = soc_esp32::APB_HZ;

/// High-speed channels.
pub const CHANNELS: u8 = 8;
/// High-speed timers.
pub const TIMERS: u8 = 4;

/// Widest duty resolution the timer's `DUTY_RES` field allows for a period
/// that still fits `HPOINT`.
pub const MAX_RES_BITS: u8 = 20;

const fn ch_conf0(ch: u8) -> u32 {
    LEDC_BASE + 0x14 * ch as u32
}
const fn ch_hpoint(ch: u8) -> u32 {
    ch_conf0(ch) + 0x04
}
const fn ch_duty(ch: u8) -> u32 {
    ch_conf0(ch) + 0x08
}
const fn ch_conf1(ch: u8) -> u32 {
    ch_conf0(ch) + 0x0C
}

const fn timer_conf(t: u8) -> u32 {
    LEDC_BASE + 0x140 + 0x08 * t as u32
}
const fn timer_value(t: u8) -> u32 {
    timer_conf(t) + 0x04
}

// CONF0 fields.
const CONF0_TIMER_SEL_MASK: u32 = 0b11;
const CONF0_SIG_OUT_EN: u32 = 1 << 2;
const CONF0_IDLE_LV: u32 = 1 << 3;

// CONF1 fields.
const CONF1_DUTY_START: u32 = 1 << 31;

// TIMER_CONF fields.
const TIMER_DUTY_RES_SHIFT: u32 = 0;
const TIMER_DIV_NUM_SHIFT: u32 = 5;
const TIMER_PAUSE: u32 = 1 << 23;
const TIMER_RST: u32 = 1 << 24;
/// 1 = APB (80 MHz), 0 = REF_TICK (1 MHz).
const TIMER_TICK_SEL_APB: u32 = 1 << 25;

/// The divider is Q10.8 — 8 fractional bits.
const DIV_FRAC_BITS: u32 = 8;
/// `LEDC_DIV_NUM_HSTIMERn` is 18 bits.
const DIV_MAX: u32 = 0x3FFFF;

/// Duty steps at `res_bits` resolution: full scale, i.e. always-on.
pub const fn max_duty(res_bits: u8) -> u32 {
    1u32 << res_bits
}

/// Q10.8 timer divider for `freq_hz` at `res_bits` resolution, or `None` if
/// the combination is out of range.
///
/// `freq = APB / (div/256 * 2^res)`, so `div = APB * 256 / (freq * 2^res)`.
///
/// Returning `None` rather than clamping is deliberate: a clamped divider
/// silently produces a different frequency, and a servo driven at the wrong
/// frequency moves to the wrong place rather than failing.
pub const fn divider_for(freq_hz: u32, res_bits: u8) -> Option<u32> {
    if freq_hz == 0 || res_bits == 0 || res_bits > MAX_RES_BITS {
        return None;
    }
    // 64-bit throughout: APB << 8 already overflows nothing, but freq << res
    // does for large resolutions.
    let period_ticks = (freq_hz as u64) << res_bits;
    if period_ticks == 0 {
        return None;
    }
    let div = ((APB_HZ as u64) << DIV_FRAC_BITS) / period_ticks;
    // Below 256 the divider is under 1.0, which the hardware cannot do.
    if div < (1 << DIV_FRAC_BITS) || div > DIV_MAX as u64 {
        return None;
    }
    Some(div as u32)
}

/// The frequency a divider actually produces, for reporting what was asked
/// against what was got.
pub const fn freq_for(div: u32, res_bits: u8) -> u32 {
    if div == 0 || res_bits > MAX_RES_BITS {
        return 0;
    }
    (((APB_HZ as u64) << DIV_FRAC_BITS) / (div as u64 * (1u64 << res_bits))) as u32
}

/// One of the four high-speed timers.
pub struct Timer {
    idx: u8,
}

impl Timer {
    /// Configure and start a timer.
    ///
    /// # Safety
    /// Takes ownership of timer `idx`'s registers. Channels pointed at this
    /// timer change frequency with it.
    pub unsafe fn new(idx: u8, freq_hz: u32, res_bits: u8) -> Option<Self> {
        if idx >= TIMERS {
            return None;
        }
        let div = divider_for(freq_hz, res_bits)?;
        let reg = timer_conf(idx) as *mut u32;

        // Clocked from APB. REF_TICK is 1 MHz, which caps the period far below
        // anything useful at 20-bit resolution.
        let conf = TIMER_TICK_SEL_APB
            | (div << TIMER_DIV_NUM_SHIFT)
            | ((res_bits as u32) << TIMER_DUTY_RES_SHIFT);

        // Reset before configuring, and release the reset after: a timer left
        // in reset counts nothing and every channel on it sits at its idle
        // level, which reads as "the pin is dead" rather than "the timer is
        // held".
        reg.write_volatile(conf | TIMER_RST);
        reg.write_volatile(conf & !TIMER_PAUSE);
        Some(Self { idx })
    }

    /// The live counter. Advances while the timer runs.
    ///
    /// Safe: a side-effect-free read of a timer this `Timer` owns.
    pub fn counter(&self) -> u32 {
        // SAFETY: holding a `Timer` is ownership of timer `idx`'s registers
        // (established by the unsafe `new`); this reads one of them.
        unsafe { (timer_value(self.idx) as *const u32).read_volatile() }
    }
}

/// One of the eight high-speed channels.
pub struct Channel {
    idx: u8,
    res_bits: u8,
}

impl Channel {
    /// Point channel `idx` at `timer` and start driving.
    ///
    /// The caller must have routed [`soc_esp32::gpio_matrix`]'s `LedcHs(idx)`
    /// signal to a pad; this drives nothing on its own.
    ///
    /// # Safety
    /// Takes ownership of channel `idx`'s registers.
    pub unsafe fn new(idx: u8, timer: &Timer, res_bits: u8, duty: u32) -> Option<Self> {
        if idx >= CHANNELS || res_bits == 0 || res_bits > MAX_RES_BITS {
            return None;
        }
        // Start of the high phase. Zero means every channel rises together,
        // which is fine here and is what makes duty alone describe the output.
        (ch_hpoint(idx) as *mut u32).write_volatile(0);

        let ch = Self { idx, res_bits };
        ch.set_duty(duty)?;

        // Idle low: a channel that idles high holds an LED on, or a motor
        // driver enabled, whenever the timer is paused.
        // Idle low, spelled so it actually happens: clear CONF0_IDLE_LV on the
        // whole composed value. An earlier `| CONF0_SIG_OUT_EN & !CONF0_IDLE_LV`
        // bound as `| (CONF0_SIG_OUT_EN & !CONF0_IDLE_LV)` by precedence, an
        // inert no-op that worked only because this is a full-register write.
        (ch_conf0(idx) as *mut u32).write_volatile(
            (((timer.idx as u32) & CONF0_TIMER_SEL_MASK) | CONF0_SIG_OUT_EN) & !CONF0_IDLE_LV,
        );
        Some(ch)
    }

    /// Set the duty, in steps of `1 / 2^res_bits`.
    ///
    /// Returns `None` if `duty` exceeds full scale — a wrapped duty is a bright
    /// LED reading as a dim one.
    ///
    /// Safe: writes only the duty registers of a channel this `Channel` owns.
    pub fn set_duty(&self, duty: u32) -> Option<()> {
        if duty > max_duty(self.res_bits) {
            return None;
        }
        // SAFETY: holding a `Channel` is ownership of channel `idx`'s registers
        // (established by the unsafe `new`); this writes two of them.
        unsafe {
            // The register's low four bits are fractional; the duty starts at
            // bit 4. See the module header.
            (ch_duty(self.idx) as *mut u32).write_volatile(duty << 4);
            // Latch it. Without this the new value sits in the register unused.
            (ch_conf1(self.idx) as *mut u32).write_volatile(CONF1_DUTY_START);
        }
        Some(())
    }

    /// Duty as a percentage of full scale, rounded to the nearest step.
    ///
    /// Safe: see [`Channel::set_duty`].
    pub fn set_percent(&self, pct: u8) -> Option<()> {
        self.set_duty(duty_for_percent(pct, self.res_bits))
    }

    /// Read back the duty the hardware holds, undoing the shift.
    ///
    /// Safe: a side-effect-free read of a channel this `Channel` owns.
    pub fn duty(&self) -> u32 {
        // SAFETY: holding a `Channel` is ownership of channel `idx`'s registers;
        // this reads one of them.
        unsafe { (ch_duty(self.idx) as *const u32).read_volatile() >> 4 }
    }
}

/// Duty steps for a percentage, rounded to nearest and clamped at 100.
pub const fn duty_for_percent(pct: u8, res_bits: u8) -> u32 {
    let pct = if pct > 100 { 100 } else { pct } as u64;
    let full = max_duty(res_bits) as u64;
    ((full * pct + 50) / 100) as u32
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registers_are_where_the_header_says() {
        assert_eq!(LEDC_BASE, 0x3FF5_9000);
        assert_eq!(ch_conf0(0), LEDC_BASE);
        assert_eq!(ch_conf0(1), LEDC_BASE + 0x14, "five registers per channel");
        assert_eq!(ch_conf0(2), LEDC_BASE + 0x28);
        assert_eq!(ch_hpoint(0), LEDC_BASE + 0x04);
        assert_eq!(ch_duty(0), LEDC_BASE + 0x08);
        assert_eq!(ch_conf1(0), LEDC_BASE + 0x0C);
        assert_eq!(timer_conf(0), LEDC_BASE + 0x140);
        assert_eq!(timer_conf(1), LEDC_BASE + 0x148);
        assert_eq!(timer_value(0), LEDC_BASE + 0x144);
    }

    #[test]
    fn channels_do_not_overlap_the_timers() {
        // Eight channels of 0x14 end at 0xA0, which is where the low-speed
        // channels begin -- so an off-by-one in the stride writes into them.
        assert_eq!(ch_conf0(7) + 0x14, LEDC_BASE + 0xA0);
        assert!(ch_conf0(7) < timer_conf(0));
    }

    #[test]
    fn a_divider_of_one_is_the_floor() {
        // Q10.8: 256 is 1.0. Below that the hardware cannot divide, and a
        // value under 256 would be read as a fraction of a tick.
        assert_eq!(divider_for(u32::MAX, 8), None, "impossibly fast");
        let d = divider_for(APB_HZ / 256, 8).unwrap();
        assert!(d >= 256, "divider {d} is below 1.0");
    }

    #[test]
    fn a_frequency_that_needs_no_division_is_refused_not_clamped() {
        // Clamping would silently produce a different frequency. A servo at
        // the wrong frequency moves to the wrong place rather than failing.
        assert_eq!(divider_for(1_000_000, 16), None);
        assert_eq!(divider_for(0, 8), None);
        assert_eq!(divider_for(1000, 0), None);
        assert_eq!(divider_for(1000, MAX_RES_BITS + 1), None);
    }

    #[test]
    fn a_common_led_frequency_round_trips() {
        // 5 kHz at 13-bit is the combination every ESP32 tutorial uses, so it
        // is the one most likely to be compared against.
        let res = 13;
        let div = divider_for(5_000, res).unwrap();
        let got = freq_for(div, res);
        assert!(got.abs_diff(5_000) < 50, "asked 5000 Hz, got {got}");
    }

    #[test]
    fn a_servo_frequency_round_trips() {
        // 50 Hz at 16-bit: the other combination in every servo example.
        let res = 16;
        let div = divider_for(50, res).unwrap();
        let got = freq_for(div, res);
        assert!(got.abs_diff(50) <= 1, "asked 50 Hz, got {got}");
    }

    #[test]
    fn full_scale_is_one_step_past_the_maximum_duty() {
        // 2^res, not 2^res - 1: the hardware treats duty == 2^res as always
        // on, and off-by-one here is a pin that never quite reaches full.
        assert_eq!(max_duty(8), 256);
        assert_eq!(max_duty(13), 8192);
        assert_eq!(duty_for_percent(100, 8), 256);
        assert_eq!(duty_for_percent(0, 8), 0);
        assert_eq!(duty_for_percent(50, 8), 128);
    }

    #[test]
    fn percentages_round_rather_than_truncate() {
        // At 8-bit, 33% is 84.48 steps. Truncating loses half a percent at
        // every step, which compounds across a fade.
        assert_eq!(duty_for_percent(33, 8), 84);
        assert_eq!(duty_for_percent(66, 8), 169);
        assert_eq!(duty_for_percent(200, 8), 256, "clamped, not wrapped");
    }

    #[test]
    fn the_conf0_fields_do_not_collide() {
        assert_eq!(CONF0_TIMER_SEL_MASK & CONF0_SIG_OUT_EN, 0);
        assert_eq!(CONF0_SIG_OUT_EN & CONF0_IDLE_LV, 0);
        // Four timers need two bits; a third would run into SIG_OUT_EN.
        assert_eq!(CONF0_TIMER_SEL_MASK, 0b11);
        assert!(TIMERS as u32 <= CONF0_TIMER_SEL_MASK + 1);
    }

    #[test]
    fn the_timer_fields_do_not_collide() {
        let div = DIV_MAX << TIMER_DIV_NUM_SHIFT;
        let res = 0x1Fu32 << TIMER_DUTY_RES_SHIFT;
        assert_eq!(div & res, 0);
        assert_eq!(div & TIMER_PAUSE, 0, "an 18-bit divider must clear PAUSE");
        assert_eq!(div & TIMER_RST, 0);
        assert_eq!(div & TIMER_TICK_SEL_APB, 0);
    }
}
