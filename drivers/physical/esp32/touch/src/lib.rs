// SPDX-License-Identifier: Apache-2.0

//! Capacitive touch sensor: ten channels (T0–T9) sharing pads with GPIO.
//!
//! Each channel measures a pad's capacitance by counting how many fixed-slope
//! charge/discharge cycles fit into a fixed measurement window: a finger adds
//! capacitance, the pad charges slower, and the count **drops**. This driver
//! covers the software-triggered, polled single measurement — configure once,
//! trigger one conversion, wait for it, read the count — which is all a button
//! poll or a self-test needs.
//!
//! # Not covered
//!
//! The FSM timer that scans channels on its own, the touch interrupt and its
//! threshold comparators, and touch as a deep-sleep wake source (a separate
//! RTC path, deferred alongside the other wake sources in `soc_esp32::sleep`).
//! This is the on-demand measurement primitive; the rest builds on it.
//!
//! # Two hardware quirks, both load-bearing
//!
//! - **Channels 8 and 9 are swapped** in the enable mask, the status bits, and
//!   the readout — but *not* in the per-pad slope register. [`Channel::swapped`]
//!   applies the swap where the hardware wants it; getting it wrong reads the
//!   wrong pad for T8/T9 and is silent on T0–T7.
//! - **Even channel → high half-word, odd channel → low half-word** of the
//!   shared `SAR_TOUCH_OUT` pair. A wrong half reads a neighbour's count.
//!
//! # Register facts
//!
//! `DR_REG_RTCIO_BASE` = `0x3FF48400`, `DR_REG_SENS_BASE` = `0x3FF48800`, from
//! esp-idf `soc/soc.h`. Fields from `soc/sens_reg.h` and `soc/rtc_io_reg.h`; the
//! sequence from `hal/esp32/include/hal/touch_sensor_ll.h` and
//! `driver/esp32/touch_sensor.c`. A SW single-shot touches only SENS and RTCIO
//! — no RTCCNTL, no FSM timer, no interrupts.
//!
//! | Register | Address | Fields used |
//! |---|---|---|
//! | `SENS_SAR_TOUCH_CTRL1` | `0x3FF48858` | `MEAS_DELAY` [15:0], `XPD_WAIT` [23:16] |
//! | `SENS_SAR_TOUCH_CTRL2` | `0x3FF48884` | `MEAS_DONE` 10, `START_FSM_EN` 11, `START_EN` 12, `START_FORCE` 13 |
//! | `SENS_SAR_TOUCH_ENABLE` | `0x3FF4888C` | `PAD_WORKEN` [9:0] |
//! | `SENS_SAR_TOUCH_OUT1..5` | `0x3FF48870`+4k | 16-bit count per channel, hi/lo half |
//! | `RTC_IO_TOUCH_CFG` | `0x3FF48490` | `DREFL` [28:27], `DRANGE` [26:25], `DREFH` [30:29] |
//! | `RTC_IO_TOUCH_PAD0..9` | `0x3FF48494`+4N | `DAC` [25:23], `TIE_OPT` 21, `MUX_SEL` 19, `FUN_IE` 13, `RUE` 27, `RDE` 28 |

#![no_std]

const RTCIO_BASE: u32 = 0x3FF4_8400;
const SENS_BASE: u32 = 0x3FF4_8800;

// ── SENS (measurement) registers ─────────────────────────────────────────────

/// `SENS_SAR_TOUCH_CTRL1_REG`: measurement window and XPD wait.
const SAR_TOUCH_CTRL1: u32 = SENS_BASE + 0x58;
const MEAS_DELAY_MASK: u32 = 0xFFFF; // [15:0]
const XPD_WAIT_SHIFT: u32 = 16;
const XPD_WAIT_MASK: u32 = 0xFF << XPD_WAIT_SHIFT; // [23:16]

/// `SENS_SAR_TOUCH_CTRL2_REG`: the SW trigger and its done flag.
const SAR_TOUCH_CTRL2: u32 = SENS_BASE + 0x84;
const MEAS_DONE: u32 = 1 << 10; // RO: conversion complete
const START_FSM_EN: u32 = 1 << 11;
const START_EN: u32 = 1 << 12; // 0->1 edge triggers one measurement
const START_FORCE: u32 = 1 << 13; // 1 = software mode

/// `SENS_SAR_TOUCH_ENABLE_REG`: per-channel measurement-enable mask, bits [9:0].
const SAR_TOUCH_ENABLE: u32 = SENS_BASE + 0x8C;

/// `SENS_SAR_TOUCH_OUT1_REG`: first of five count-pair registers, 4 bytes apart.
/// Channel N's count is in pair `N/2`; even N in the high half-word, odd N in
/// the low half-word.
const SAR_TOUCH_OUT1: u32 = SENS_BASE + 0x70;

// ── RTCIO (pad) registers ─────────────────────────────────────────────────────

/// `RTC_IO_TOUCH_CFG_REG`: the reference voltages the count is measured against.
const RTC_IO_TOUCH_CFG: u32 = RTCIO_BASE + 0x90;
const DRANGE_SHIFT: u32 = 25; // atten, [26:25]
const DREFL_SHIFT: u32 = 27; // low voltage, [28:27]
const DREFH_SHIFT: u32 = 29; // high voltage, [30:29]
const DREF_FIELD_MASK: u32 = (0x3 << DREFH_SHIFT) | (0x3 << DREFL_SHIFT) | (0x3 << DRANGE_SHIFT);

/// `RTC_IO_TOUCH_PAD0_REG`: first per-pad register; pad N is `+ 4*N`.
const RTC_IO_TOUCH_PAD0: u32 = RTCIO_BASE + 0x94;
const PAD_FUN_IE: u32 = 1 << 13;
const PAD_FUN_SEL_MASK: u32 = 0x3 << 17;
const PAD_MUX_SEL: u32 = 1 << 19; // 0 = RTC/touch function; must be 0 to sense
const PAD_TIE_OPT: u32 = 1 << 21; // initial charge level; 0 = TIE_OPT_LOW
const PAD_DAC_SHIFT: u32 = 23; // slope, [25:23]
const PAD_DAC_MASK: u32 = 0x7 << PAD_DAC_SHIFT;
const PAD_RUE: u32 = 1 << 27;
const PAD_RDE: u32 = 1 << 28;

// ── Configuration defaults (esp-idf enum values, not names) ───────────────────

/// `TOUCH_PAD_MEASURE_CYCLE_DEFAULT`: the measurement window in 8 MHz cycles.
const MEAS_DELAY_DEFAULT: u32 = 0x7FFF;
/// `SOC_TOUCH_PAD_MEASURE_WAIT_MAX`: settle time before a measurement.
const XPD_WAIT_DEFAULT: u32 = 0xFF;
/// `TOUCH_HVOLT_2V7` (3), `TOUCH_LVOLT_0V5` (0), `TOUCH_HVOLT_ATTEN_1V` (1).
const DREFH_2V7: u32 = 3;
const DREFL_0V5: u32 = 0;
const DRANGE_ATTEN_1V: u32 = 1;
/// `TOUCH_PAD_SLOPE_7` (7) and `TOUCH_PAD_TIE_OPT_LOW` (0) — esp-idf's defaults.
const SLOPE_DEFAULT: u32 = 7;

/// How long to wait for a conversion. A measurement at the default window is
/// well under a millisecond at 8 MHz; this absorbs interrupts and still fails a
/// dead controller rather than spinning forever.
const MEAS_POLL_SPINS: u32 = 1_000_000;

/// A touch channel, named by its `Tn` number. Each maps to one fixed GPIO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Channel {
    T0 = 0,
    T1 = 1,
    T2 = 2,
    T3 = 3,
    T4 = 4,
    T5 = 5,
    T6 = 6,
    T7 = 7,
    T8 = 8,
    T9 = 9,
}

impl Channel {
    /// The GPIO this channel senses (`touch_sensor_channel.h`).
    pub const fn gpio(self) -> u8 {
        match self {
            Channel::T0 => 4,
            Channel::T1 => 0,
            Channel::T2 => 2,
            Channel::T3 => 15,
            Channel::T4 => 13,
            Channel::T5 => 12,
            Channel::T6 => 14,
            Channel::T7 => 27,
            Channel::T8 => 33,
            Channel::T9 => 32,
        }
    }

    /// The channel that senses `gpio`, if any of the ten pads does.
    pub const fn from_gpio(gpio: u8) -> Option<Self> {
        Some(match gpio {
            4 => Channel::T0,
            0 => Channel::T1,
            2 => Channel::T2,
            15 => Channel::T3,
            13 => Channel::T4,
            12 => Channel::T5,
            14 => Channel::T6,
            27 => Channel::T7,
            33 => Channel::T8,
            32 => Channel::T9,
            _ => return None,
        })
    }

    /// The raw channel number, 0–9 — the index for the per-pad slope register.
    const fn num(self) -> u32 {
        self as u32
    }

    /// The number with 8↔9 swapped, as the enable mask, status bits, readout,
    /// and per-pad tie-option register all index. Channels 0–7 are unchanged.
    const fn swapped(self) -> u32 {
        match self {
            Channel::T8 => 9,
            Channel::T9 => 8,
            other => other as u32,
        }
    }

    /// The `SAR_TOUCH_OUT` register holding this channel's count, and the shift
    /// to its 16-bit half (even channel = high half-word, odd = low).
    const fn out_reg_and_shift(self) -> (u32, u32) {
        let s = self.swapped();
        let reg = SAR_TOUCH_OUT1 + (s / 2) * 4;
        let shift = if s % 2 == 0 { 16 } else { 0 };
        (reg, shift)
    }
}

unsafe fn read(addr: u32) -> u32 {
    (addr as *mut u32).read_volatile()
}

unsafe fn write(addr: u32, val: u32) {
    (addr as *mut u32).write_volatile(val);
}

unsafe fn modify(addr: u32, mask: u32, value: u32) {
    let a = addr as *mut u32;
    a.write_volatile((a.read_volatile() & !mask) | value);
}

/// A conversion could not be completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchError {
    /// The done flag never set within the poll bound: the FSM is not running or
    /// the trigger did not take.
    Timeout,
}

/// The touch controller. One owner keeps the shared measurement registers
/// consistent across channels.
pub struct Touch {
    _private: (),
}

impl Touch {
    /// Bring the controller up for software-triggered measurements.
    ///
    /// Sets the measurement window and reference voltages, selects software
    /// (not timer) mode, and clears the channel-enable mask. Per-pad slope and
    /// routing are applied in [`read`](Self::read), when the channel is known.
    ///
    /// # Safety
    /// Takes exclusive ownership of the SENS touch and RTCIO touch registers.
    pub unsafe fn new() -> Self {
        // Measurement window and XPD settle.
        modify(
            SAR_TOUCH_CTRL1,
            MEAS_DELAY_MASK | XPD_WAIT_MASK,
            MEAS_DELAY_DEFAULT | (XPD_WAIT_DEFAULT << XPD_WAIT_SHIFT),
        );
        // Reference voltages: high 2.7 V, low 0.5 V, 1 V attenuation.
        modify(
            RTC_IO_TOUCH_CFG,
            DREF_FIELD_MASK,
            (DREFH_2V7 << DREFH_SHIFT) | (DREFL_0V5 << DREFL_SHIFT) | (DRANGE_ATTEN_1V << DRANGE_SHIFT),
        );
        // Software mode: FSM enabled, driven by the START_EN edge, not the timer.
        modify(
            SAR_TOUCH_CTRL2,
            START_FSM_EN | START_FORCE | START_EN,
            START_FSM_EN | START_FORCE,
        );
        // No channel enabled for measurement yet.
        write(SAR_TOUCH_ENABLE, 0);
        Self { _private: () }
    }

    /// Run one software-triggered measurement on `ch` and return its raw count.
    ///
    /// A finger lowers the count; an untouched pad returns its steady parasitic
    /// value. `0` is the controller's own "not ready / error" sentinel.
    ///
    /// # Safety
    /// Drives the touch measurement registers and the channel's pad.
    pub unsafe fn read(&self, ch: Channel) -> Result<u16, TouchError> {
        // Route the pad to the touch/RTC function and set its charge slope. The
        // slope register is indexed by the raw number; the tie-option by the
        // swapped one (the 8/9 quirk), so they are written separately.
        let pad = RTC_IO_TOUCH_PAD0 + ch.num() * 4;
        modify(
            pad,
            PAD_FUN_IE | PAD_FUN_SEL_MASK | PAD_MUX_SEL | PAD_RUE | PAD_RDE | PAD_DAC_MASK,
            SLOPE_DEFAULT << PAD_DAC_SHIFT, // mux_sel/fun_ie/pulls all cleared to 0
        );
        // TIE_OPT_LOW on the swapped pad.
        modify(RTC_IO_TOUCH_PAD0 + ch.swapped() * 4, PAD_TIE_OPT, 0);

        // Enable just this channel for measurement.
        let mask = 1 << ch.swapped();
        modify(SAR_TOUCH_ENABLE, mask, mask);

        // Trigger one measurement on the 0->1 edge of START_EN.
        modify(SAR_TOUCH_CTRL2, START_EN, 0);
        modify(SAR_TOUCH_CTRL2, START_EN, START_EN);

        // Wait for the conversion to complete.
        let mut spins = MEAS_POLL_SPINS;
        while read(SAR_TOUCH_CTRL2) & MEAS_DONE == 0 {
            spins -= 1;
            if spins == 0 {
                modify(SAR_TOUCH_ENABLE, mask, 0);
                return Err(TouchError::Timeout);
            }
            core::hint::spin_loop();
        }

        // Read the count from the correct half-word, then release the channel.
        let (out_reg, shift) = ch.out_reg_and_shift();
        let count = ((read(out_reg) >> shift) & 0xFFFF) as u16;
        modify(SAR_TOUCH_ENABLE, mask, 0);
        Ok(count)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_addresses_match_the_headers() {
        assert_eq!(SAR_TOUCH_CTRL1, 0x3FF4_8858);
        assert_eq!(SAR_TOUCH_CTRL2, 0x3FF4_8884);
        assert_eq!(SAR_TOUCH_ENABLE, 0x3FF4_888C);
        assert_eq!(SAR_TOUCH_OUT1, 0x3FF4_8870);
        assert_eq!(RTC_IO_TOUCH_CFG, 0x3FF4_8490);
        assert_eq!(RTC_IO_TOUCH_PAD0, 0x3FF4_8494);
        // Pad N is 4 bytes apart; T7's pad and T9's pad.
        assert_eq!(RTC_IO_TOUCH_PAD0 + Channel::T7.num() * 4, 0x3FF4_84B0);
        assert_eq!(RTC_IO_TOUCH_PAD0 + 9 * 4, 0x3FF4_84B8);
    }

    #[test]
    fn the_channel_gpio_map_is_the_esp_idf_one_both_ways() {
        let pairs = [
            (Channel::T0, 4u8),
            (Channel::T1, 0),
            (Channel::T2, 2),
            (Channel::T3, 15),
            (Channel::T4, 13),
            (Channel::T5, 12),
            (Channel::T6, 14),
            (Channel::T7, 27),
            (Channel::T8, 33),
            (Channel::T9, 32),
        ];
        for (ch, gpio) in pairs {
            assert_eq!(ch.gpio(), gpio);
            assert_eq!(Channel::from_gpio(gpio), Some(ch));
        }
        // A pad that cannot sense touch is rejected — including the DevKitC's
        // general loopback pads.
        for gpio in [1u8, 3, 5, 16, 19, 21, 22, 23, 25, 26] {
            assert_eq!(Channel::from_gpio(gpio), None, "GPIO{gpio} was accepted");
        }
    }

    #[test]
    fn channels_8_and_9_swap_but_the_rest_do_not() {
        assert_eq!(Channel::T8.num(), 8);
        assert_eq!(Channel::T8.swapped(), 9);
        assert_eq!(Channel::T9.num(), 9);
        assert_eq!(Channel::T9.swapped(), 8);
        for ch in [Channel::T0, Channel::T5, Channel::T7] {
            assert_eq!(ch.num(), ch.swapped(), "channel 0–7 must not swap");
        }
    }

    #[test]
    fn the_count_is_read_from_the_right_pair_and_half() {
        // Even channel -> high half; odd -> low half; pair = swapped/2.
        assert_eq!(Channel::T0.out_reg_and_shift(), (0x3FF4_8870, 16));
        assert_eq!(Channel::T1.out_reg_and_shift(), (0x3FF4_8870, 0));
        assert_eq!(Channel::T7.out_reg_and_shift(), (0x3FF4_887C, 0)); // 7 odd -> low
        // The 8/9 swap moves T8 to slot 9 (low of OUT5) and T9 to slot 8 (high).
        assert_eq!(Channel::T8.out_reg_and_shift(), (0x3FF4_8880, 0));
        assert_eq!(Channel::T9.out_reg_and_shift(), (0x3FF4_8880, 16));
    }

    #[test]
    fn the_control_bits_are_the_documented_positions() {
        assert_eq!(MEAS_DONE, 1 << 10);
        assert_eq!(START_FSM_EN, 1 << 11);
        assert_eq!(START_EN, 1 << 12);
        assert_eq!(START_FORCE, 1 << 13);
        assert_eq!(XPD_WAIT_MASK, 0xFF << 16);
        // Pad fields must not collide: slope [25:23] clear of mux_sel(19)/tie(21).
        assert_eq!(PAD_DAC_MASK, 0x7 << 23);
        assert_eq!(PAD_DAC_MASK & (PAD_MUX_SEL | PAD_TIE_OPT), 0);
        // The voltage fields are three distinct 2-bit fields.
        assert_eq!(DREF_FIELD_MASK, (0x3 << 29) | (0x3 << 27) | (0x3 << 25));
    }
}
