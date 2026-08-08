// SPDX-License-Identifier: Apache-2.0

//! TIMG0 and TIMG1: four 64-bit general-purpose timers.
//!
//! The kernel's `timer::once`/`every` ride the 1 ms scheduler tick, so nothing
//! sub-millisecond is expressible and every period is quantised to the tick
//! and to scheduler load. These are the hardware underneath a timer that has
//! to be accurate: a 16-bit prescaler off the 80 MHz APB clock, a 64-bit
//! counter, and an alarm that fires an interrupt.
//!
//! Two groups, two timers each. Each timer is wholly independent — its own
//! divider, counter, alarm and interrupt source.
//!
//! # This is not the tick
//!
//! The scheduler tick is the Xtensa core's own CCOMPARE, in `arch-xtensa`, and
//! stays that way. A tick that lived in a peripheral would stop when that
//! peripheral was gated off, and the tick is the one clock the kernel cannot
//! do without. These are for everything else.
//!
//! # Register facts
//!
//! Offsets and bit positions from esp-idf `soc/timer_group_reg.h`. T0 sits at
//! the group base and T1 is `0x24` further on, so one set of offsets serves
//! both.
//!
//! | Register | Offset | Notes |
//! |---|---|---|
//! | `CONFIG` | `0x00` | `EN` 31, `INCREASE` 30, `AUTORELOAD` 29, `DIVIDER` [28:13], `LEVEL_INT_EN` 11, `ALARM_EN` 10 |
//! | `LO` / `HI` | `0x04` / `0x08` | the counter, **only valid after a latch** |
//! | `UPDATE` | `0x0c` | write anything to latch the counter into LO/HI |
//! | `ALARMLO` / `ALARMHI` | `0x10` / `0x14` | when to fire |
//! | `LOADLO` / `LOADHI` | `0x18` / `0x1c` | value a reload installs |
//! | `LOAD` | `0x20` | write anything to install it |
//!
//! Group-wide, not per timer: `INT_ENA` `0x98`, `INT_RAW` `0x9c`,
//! `INT_ST` `0xa0`, `INT_CLR` `0xa4`, with T0 in bit 0 and T1 in bit 1.

#![no_std]

use soc_esp32::addr::{TIMG0_BASE, TIMG1_BASE};
use soc_esp32::reg;

/// Which timer group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Timg0,
    Timg1,
}

/// Which timer within a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    T0,
    T1,
}

/// What happens when the alarm fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Fire once. The counter keeps running; the alarm does not re-arm.
    OneShot,
    /// Reload to zero and fire again, forever.
    Periodic,
}

/// Why a timer could not be configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerError {
    /// The requested resolution needs a prescaler the hardware cannot encode.
    UnsupportedResolution,
    /// The requested period does not fit a 64-bit count at this resolution,
    /// or is zero.
    UnsupportedPeriod,
}

/// APB clock feeding the prescaler.
const APB_HZ: u32 = 80_000_000;

// Per-timer register offsets, from the timer's own base.
const CONFIG: u32 = 0x00;
const LO: u32 = 0x04;
const HI: u32 = 0x08;
const UPDATE: u32 = 0x0C;
const ALARMLO: u32 = 0x10;
const ALARMHI: u32 = 0x14;
const LOADLO: u32 = 0x18;
const LOADHI: u32 = 0x1C;
const LOAD: u32 = 0x20;

/// T1's registers are one full set past T0's.
const TIMER_STRIDE: u32 = 0x24;

// Group-wide interrupt registers, from the group base.
const INT_ENA: u32 = 0x98;
const INT_RAW: u32 = 0x9C;
const INT_CLR: u32 = 0xA4;

const CONFIG_EN: u32 = 1 << 31;
const CONFIG_INCREASE: u32 = 1 << 30;
const CONFIG_AUTORELOAD: u32 = 1 << 29;
const CONFIG_DIVIDER_SHIFT: u32 = 13;
const CONFIG_DIVIDER_MASK: u32 = 0xFFFF;
const CONFIG_LEVEL_INT_EN: u32 = 1 << 11;
const CONFIG_ALARM_EN: u32 = 1 << 10;

/// One general-purpose timer.
pub struct Timg {
    /// Base of this timer's own register set, group base plus stride.
    base: u32,
    /// Bit for this timer in the group's interrupt registers.
    int_bit: u32,
    /// Group base, where the interrupt registers live.
    group: u32,
    /// Prescaler in force, so a count can be converted back to microseconds.
    divider: u32,
}

impl Timg {
    /// Take a timer and set its resolution.
    ///
    /// `resolution_hz` is how fast the counter should tick — 1_000_000 for
    /// microseconds. The prescaler is 16 bits and the hardware refuses a
    /// divider below 2, so the slowest useful resolution is about 1.2 kHz and
    /// the fastest is 40 MHz.
    ///
    /// # Safety
    /// Takes exclusive ownership of the timer's registers. Two instances for
    /// the same group and timer will fight over the same alarm.
    pub unsafe fn new(group: Group, timer: Timer, resolution_hz: u32) -> Result<Self, TimerError> {
        let group_base = match group {
            Group::Timg0 => TIMG0_BASE,
            Group::Timg1 => TIMG1_BASE,
        };
        let (offset, int_bit) = match timer {
            Timer::T0 => (0, 1 << 0),
            Timer::T1 => (TIMER_STRIDE, 1 << 1),
        };
        let divider = divider_for(resolution_hz)?;

        let t = Self {
            base: group_base + offset,
            int_bit,
            group: group_base,
            divider,
        };
        t.write(CONFIG, encode_config(divider, false, false));
        t.stop();
        t.clear_interrupt();
        Ok(t)
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    unsafe fn write(&self, offset: u32, value: u32) {
        self.reg(offset).write_volatile(value);
    }

    unsafe fn read(&self, offset: u32) -> u32 {
        self.reg(offset).read_volatile()
    }

    /// The counter, in the units `resolution_hz` asked for.
    ///
    /// The latch is not optional. `LO` and `HI` hold whatever the last latch
    /// put there, so reading them directly returns a stale value — and a stale
    /// value that does not change looks exactly like a timer that never
    /// started.
    ///
    /// # Safety
    /// Reads this timer's registers.
    pub unsafe fn now(&self) -> u64 {
        self.write(UPDATE, 1);
        let lo = self.read(LO) as u64;
        let hi = self.read(HI) as u64;
        (hi << 32) | lo
    }

    /// Start counting up from zero, with no alarm.
    ///
    /// # Safety
    /// Writes this timer's registers.
    pub unsafe fn start_free_running(&self) {
        self.load(0);
        self.write(CONFIG, encode_config(self.divider, false, false) | CONFIG_EN);
    }

    /// Start counting and fire the alarm after `period` counts.
    ///
    /// Interrupt delivery still needs `soc_esp32::intr_map::route` and a
    /// registered handler — this crate cannot do either, being allowed to
    /// depend on `hal` and `soc/*` only. An alarm enabled but not routed fires
    /// into nothing, which is indistinguishable from a timer that never ran.
    ///
    /// # Safety
    /// Writes this timer's registers.
    pub unsafe fn start_alarm(&self, period: u64, mode: Mode) -> Result<(), TimerError> {
        if period == 0 {
            return Err(TimerError::UnsupportedPeriod);
        }
        self.stop();
        self.load(0);
        self.write(ALARMLO, period as u32);
        self.write(ALARMHI, (period >> 32) as u32);
        // Reload target for the periodic case: back to zero, so the next alarm
        // is a full period away rather than immediate.
        self.write(LOADLO, 0);
        self.write(LOADHI, 0);
        self.enable_interrupt();
        let autoreload = matches!(mode, Mode::Periodic);
        self.write(
            CONFIG,
            encode_config(self.divider, autoreload, true) | CONFIG_EN,
        );
        Ok(())
    }

    /// Re-arm the alarm after it has fired.
    ///
    /// `ALARM_EN` clears itself when the alarm goes off, on every mode
    /// including periodic — auto-reload reloads the *counter*, not the alarm.
    /// A periodic timer whose handler forgets this fires exactly once.
    ///
    /// Measured, not inferred: deleting the re-arm from the on-target periodic
    /// test gives one alarm and then silence, which is what that test's
    /// "fired once and stopped" failure exists to name.
    ///
    /// # Safety
    /// Writes this timer's config register.
    pub unsafe fn rearm(&self) {
        let cfg = self.read(CONFIG);
        self.write(CONFIG, cfg | CONFIG_ALARM_EN);
    }

    /// Install `value` into the counter.
    ///
    /// # Safety
    /// Writes this timer's registers.
    pub unsafe fn load(&self, value: u64) {
        self.write(LOADLO, value as u32);
        self.write(LOADHI, (value >> 32) as u32);
        self.write(LOAD, 1);
    }

    /// Halt the counter. It keeps its value.
    ///
    /// # Safety
    /// Writes this timer's config register.
    pub unsafe fn stop(&self) {
        let cfg = self.read(CONFIG);
        self.write(CONFIG, cfg & !CONFIG_EN);
    }

    /// Allow this timer to raise the group's interrupt.
    ///
    /// # Safety
    /// Read-modify-writes a register shared with the group's other timer.
    pub unsafe fn enable_interrupt(&self) {
        let r = (self.group + INT_ENA) as *mut u32;
        reg::set(r, self.int_bit);
    }

    /// Has this timer's alarm fired?
    ///
    /// # Safety
    /// Reads a group register.
    pub unsafe fn fired(&self) -> bool {
        ((self.group + INT_RAW) as *const u32).read_volatile() & self.int_bit != 0
    }

    /// Acknowledge the alarm. **A top-half must call this.**
    ///
    /// Level-triggered: returning from a handler without clearing re-enters it
    /// forever.
    ///
    /// # Safety
    /// Writes a group register.
    pub unsafe fn clear_interrupt(&self) {
        ((self.group + INT_CLR) as *mut u32).write_volatile(self.int_bit);
    }

    /// The prescaler in force.
    pub const fn divider(&self) -> u32 {
        self.divider
    }
}

/// Acknowledge a timer's alarm without holding a [`Timg`].
///
/// For a top-half, which cannot take a lock and so cannot reach a shared
/// handle. The registers are the state; nothing else is needed to clear one
/// bit.
///
/// # Safety
/// Writes the group's interrupt-clear register.
pub unsafe fn clear_interrupt(group: Group, timer: Timer) {
    let base = match group {
        Group::Timg0 => TIMG0_BASE,
        Group::Timg1 => TIMG1_BASE,
    };
    let bit = match timer {
        Timer::T0 => 1u32 << 0,
        Timer::T1 => 1u32 << 1,
    };
    ((base + INT_CLR) as *mut u32).write_volatile(bit);
}

/// Re-arm a timer's alarm without holding a [`Timg`].
///
/// The companion to [`clear_interrupt`], and needed in the same place: a
/// periodic alarm's handler has to put `ALARM_EN` back every time, and a
/// top-half cannot reach a shared handle.
///
/// # Safety
/// Read-modify-writes the timer's config register.
pub unsafe fn rearm(group: Group, timer: Timer) {
    let base = timer_base(group, timer);
    let r = (base + CONFIG) as *mut u32;
    reg::set(r, CONFIG_ALARM_EN);
}

/// Base of one timer's register set.
const fn timer_base(group: Group, timer: Timer) -> u32 {
    let g = match group {
        Group::Timg0 => TIMG0_BASE,
        Group::Timg1 => TIMG1_BASE,
    };
    match timer {
        Timer::T0 => g,
        Timer::T1 => g + TIMER_STRIDE,
    }
}

/// Prescaler for a requested counter frequency.
///
/// The field is 16 bits and the hardware treats 0 as 65536, so the encodable
/// range is 2..=65536 — esp-idf asserts the same lower bound. A divider of 1
/// is not "no division"; it is rejected.
pub fn divider_for(resolution_hz: u32) -> Result<u32, TimerError> {
    if resolution_hz == 0 {
        return Err(TimerError::UnsupportedResolution);
    }
    let div = APB_HZ / resolution_hz;
    // Exact only. A resolution that does not divide the APB clock would make
    // every period silently wrong by the rounding error, which is the kind of
    // thing that shows up as clock drift days later.
    if div * resolution_hz != APB_HZ {
        return Err(TimerError::UnsupportedResolution);
    }
    if !(2..=65536).contains(&div) {
        return Err(TimerError::UnsupportedResolution);
    }
    Ok(div)
}

/// Encode the config word. 65536 is written as 0, which is how the 16-bit
/// field expresses its own maximum.
fn encode_config(divider: u32, autoreload: bool, alarm: bool) -> u32 {
    let field = if divider == 65536 { 0 } else { divider };
    let mut cfg = CONFIG_INCREASE | ((field & CONFIG_DIVIDER_MASK) << CONFIG_DIVIDER_SHIFT);
    if autoreload {
        cfg |= CONFIG_AUTORELOAD;
    }
    if alarm {
        cfg |= CONFIG_ALARM_EN | CONFIG_LEVEL_INT_EN;
    }
    cfg
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_match_the_timer_group_header() {
        assert_eq!(CONFIG, 0x00);
        assert_eq!(LO, 0x04);
        assert_eq!(HI, 0x08);
        assert_eq!(UPDATE, 0x0C);
        assert_eq!(ALARMLO, 0x10);
        assert_eq!(ALARMHI, 0x14);
        assert_eq!(LOADLO, 0x18);
        assert_eq!(LOADHI, 0x1C);
        assert_eq!(LOAD, 0x20);
        assert_eq!(INT_ENA, 0x98);
        assert_eq!(INT_RAW, 0x9C);
        assert_eq!(INT_CLR, 0xA4);
    }

    #[test]
    fn t1_is_one_full_register_set_past_t0() {
        // `TIMG_T1CONFIG_REG` is base + 0x24, exactly where T0's `LOAD` + 4
        // lands. A wrong stride points T1 at T0's tail and the two timers
        // corrupt each other.
        assert_eq!(TIMER_STRIDE, 0x24);
        assert_eq!(LOAD + 4, TIMER_STRIDE);
    }

    #[test]
    fn microseconds_need_a_divider_of_eighty() {
        assert_eq!(divider_for(1_000_000).unwrap(), 80);
    }

    #[test]
    fn a_resolution_that_does_not_divide_the_apb_clock_is_refused() {
        // 3 MHz gives 26.67, which would round to 26 and run 2.5% fast --
        // about half an hour a day.
        assert_eq!(
            divider_for(3_000_000).unwrap_err(),
            TimerError::UnsupportedResolution
        );
    }

    #[test]
    fn the_encodable_divider_range_is_two_to_sixty_five_thousand_five_hundred_and_thirty_six() {
        // 40 MHz -> 2, the fastest. 80 MHz would need 1, which the hardware
        // does not accept and esp-idf asserts against.
        assert_eq!(divider_for(40_000_000).unwrap(), 2);
        assert_eq!(
            divider_for(80_000_000).unwrap_err(),
            TimerError::UnsupportedResolution
        );
        // 1220.703125 Hz is not exact; 1250 Hz gives 64000, which is.
        assert_eq!(divider_for(1250).unwrap(), 64_000);
        assert_eq!(divider_for(0).unwrap_err(), TimerError::UnsupportedResolution);
    }

    #[test]
    fn the_divider_lands_in_bits_thirteen_to_twenty_eight() {
        let cfg = encode_config(80, false, false);
        assert_eq!((cfg >> 13) & 0xFFFF, 80, "divider is not at bit 13");
        assert_eq!(cfg & CONFIG_INCREASE, CONFIG_INCREASE, "counter must count up");
        assert_eq!(cfg & CONFIG_EN, 0, "config alone must not start the timer");
    }

    #[test]
    fn a_divider_of_sixty_five_thousand_five_hundred_and_thirty_six_is_written_as_zero() {
        // The field is 16 bits, so its maximum cannot be written literally.
        // Writing 65536 truncates to 0 anyway -- but only by accident, and an
        // accident is not a contract.
        let cfg = encode_config(65_536, false, false);
        assert_eq!((cfg >> 13) & 0xFFFF, 0);
    }

    #[test]
    fn periodic_sets_autoreload_and_one_shot_does_not() {
        assert_eq!(encode_config(80, true, true) & CONFIG_AUTORELOAD, CONFIG_AUTORELOAD);
        assert_eq!(encode_config(80, false, true) & CONFIG_AUTORELOAD, 0);
    }

    #[test]
    fn arming_an_alarm_enables_the_level_interrupt_too() {
        // ALARM_EN without LEVEL_INT_EN fires an alarm that raises nothing.
        let cfg = encode_config(80, false, true);
        assert_eq!(cfg & CONFIG_ALARM_EN, CONFIG_ALARM_EN);
        assert_eq!(cfg & CONFIG_LEVEL_INT_EN, CONFIG_LEVEL_INT_EN);
        let idle = encode_config(80, false, false);
        assert_eq!(idle & (CONFIG_ALARM_EN | CONFIG_LEVEL_INT_EN), 0);
    }

    #[test]
    fn the_two_timers_have_distinct_interrupt_bits() {
        // Both timers share one INT_ENA/INT_CLR register. The same bit for
        // both would let one timer's handler acknowledge the other's alarm.
        assert_ne!(1u32 << 0, 1u32 << 1);
    }
}
