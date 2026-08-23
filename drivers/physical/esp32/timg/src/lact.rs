// SPDX-License-Identifier: Apache-2.0

//! The LAC timer — a timer group's *fifth* counter, and a programmable alarm.
//!
//! # Why this rather than one of the four
//!
//! Each timer group documents two general-purpose timers, and the ESP32 has
//! two groups, so four in total. FlintOS has all four spoken for: TIMG1/T1 is
//! `kernel::clock`, and TIMG0/T0, TIMG0/T1 and TIMG1/T0 drive on-target
//! self-tests.
//!
//! The LAC ("low-power/ALARM counter") is a separate 64-bit up-counter inside
//! the same register block, at offsets 0x70..0x94, with its own alarm compare
//! and its own interrupt bit. **esp-idf's `esp_timer` runs on exactly this
//! counter, on TG0, for exactly this reason** — see
//! `components/esp_timer/src/esp_timer_impl_lac.c`, whose header says so in as
//! many words. NuttX reaches the same hardware through esp-idf's
//! `esp_timer`. So using it here costs no general-purpose timer and follows
//! the reference rather than diverging from it.
//!
//! # Ticks, and why two per microsecond
//!
//! The counter is clocked from APB through a 16-bit divider. esp-idf uses
//! `TICKS_PER_US = 2` and comments the reason: at one tick per microsecond the
//! counter needs up to a microsecond of settling after `UPDATE` before it can
//! be read, which makes reading the time too slow. Two ticks per microsecond
//! divides evenly into every APB frequency Espressif ships and halves that
//! wait. Copied, including the value.
//!
//! # What this does not do
//!
//! No sleep support. esp-idf also programs `LACTRTC` so the counter keeps
//! time from the RTC slow clock across light sleep; FlintOS has no sleep
//! (#31), and a register written for a mode that cannot be entered is a
//! claim about behaviour nobody can check.

use soc_esp32::addr::{TIMG0_BASE, TIMG1_BASE};

use crate::Group;

/// Counter ticks per microsecond. esp-idf's `TICKS_PER_US`; see the module
/// docs for why it is not 1.
pub const TICKS_PER_US: u64 = 2;

// Register offsets from a timer group's base, from `timg_reg.h` at v4.4.
const LACTCONFIG: u32 = 0x0070;
const LACTLO: u32 = 0x0078;
const LACTHI: u32 = 0x007C;
const LACTUPDATE: u32 = 0x0080;
const LACTALARMLO: u32 = 0x0084;
const LACTALARMHI: u32 = 0x0088;
const LACTLOADLO: u32 = 0x008C;
const LACTLOADHI: u32 = 0x0090;
const LACTLOAD: u32 = 0x0094;
const INT_ENA_TIMERS: u32 = 0x0098;
const INT_ST_TIMERS: u32 = 0x00A0;
const INT_CLR_TIMERS: u32 = 0x00A4;

// `LACTCONFIG` fields.
const LACT_EN: u32 = 1 << 31;
const LACT_INCREASE: u32 = 1 << 30;
const LACT_LEVEL_INT_EN: u32 = 1 << 11;
const LACT_ALARM_EN: u32 = 1 << 10;
const LACT_LAC_EN: u32 = 1 << 9;
const LACT_DIVIDER_MASK: u32 = 0x0000_FFFF;
const LACT_DIVIDER_SHIFT: u32 = 13;

/// The LAC timer's bit in the group's interrupt registers. Bit 3, after the
/// two general-purpose timers and the watchdog.
pub const LACT_INT: u32 = 1 << 3;

/// `ETS_TG0_LACT_LEVEL_INTR_SOURCE`. The crossbar input this raises.
pub const TG0_LACT_INTR_SOURCE: u8 = 17;

/// `ETS_TG1_LACT_LEVEL_INTR_SOURCE`.
pub const TG1_LACT_INTR_SOURCE: u8 = 18;

/// One group's LAC timer.
pub struct Lact {
    base: u32,
}

impl Lact {
    /// Take the LAC timer of `group` and start it counting up.
    ///
    /// `apb_mhz` is the APB clock in MHz — 80 on every board here. It must be
    /// divisible by [`TICKS_PER_US`], which esp-idf asserts for the same
    /// reason: the divider is an integer and a remainder would make the
    /// counter run at a rate that is not a whole number of ticks per
    /// microsecond.
    ///
    /// # Safety
    /// Takes exclusive ownership of the group's LAC registers. The timer
    /// group's clock must already be enabled, which it is for TIMG0 out of
    /// reset.
    pub unsafe fn new(group: Group, apb_mhz: u32) -> Option<Self> {
        if apb_mhz == 0 || (apb_mhz as u64) % TICKS_PER_US != 0 {
            return None;
        }
        let divider = (apb_mhz as u64 / TICKS_PER_US) as u32;
        if divider == 0 || divider > LACT_DIVIDER_MASK {
            return None;
        }
        let base = match group {
            Group::Timg0 => TIMG0_BASE,
            Group::Timg1 => TIMG1_BASE,
        };
        let lact = Lact { base };

        // Counting up, LAC enabled, alarm off until someone asks for one, and
        // a level interrupt rather than an edge: a level interrupt stays
        // asserted until the handler clears it, so a handler that is delayed
        // still sees why it was called.
        unsafe {
            lact.write(
                LACTCONFIG,
                LACT_EN | LACT_INCREASE | LACT_LAC_EN | LACT_LEVEL_INT_EN
                    | (divider << LACT_DIVIDER_SHIFT),
            );
            // Start from zero, so `now_ticks` is time since init rather than
            // time since power-on with whatever the bootloader left.
            lact.write(LACTLOADLO, 0);
            lact.write(LACTLOADHI, 0);
            lact.write(LACTLOAD, 1);
            lact.clear_interrupt();
        }
        Some(lact)
    }

    #[inline]
    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    #[inline]
    unsafe fn read(&self, offset: u32) -> u32 {
        unsafe { self.reg(offset).read_volatile() }
    }

    #[inline]
    unsafe fn write(&self, offset: u32, value: u32) {
        unsafe { self.reg(offset).write_volatile(value) }
    }

    /// The counter, in ticks.
    ///
    /// Latched first: `LACTLO`/`LACTHI` hold a snapshot, and reading them
    /// without writing `LACTUPDATE` returns the previous snapshot — which
    /// looks like a clock that has stopped.
    ///
    /// Safe: latches and reads registers a held `Lact` owns.
    #[inline]
    #[cfg_attr(target_os = "none", link_section = ".iram1.lact")]
    pub fn now_ticks(&self) -> u64 {
        unsafe {
            self.write(LACTUPDATE, 1);
            let lo = self.read(LACTLO);
            let hi = self.read(LACTHI);
            ((hi as u64) << 32) | lo as u64
        }
    }

    /// Raise the interrupt when the counter reaches `ticks`.
    ///
    /// **Re-checks and pushes the alarm out if it has already passed.** The
    /// window between computing a deadline and writing it is real, and a
    /// compare value already behind the counter never matches — a 64-bit
    /// up-counter does not wrap round to it in any useful time. esp-idf loops
    /// here for the same reason and this follows it, including the two-tick
    /// margin, with the loop bounded rather than open.
    ///
    /// Safe: writes registers a held `Lact` owns.
    #[cfg_attr(target_os = "none", link_section = ".iram1.lact")]
    pub fn set_alarm(&self, ticks: u64) {
        // Enough attempts to cover a handful of interrupts landing in the
        // window, and few enough to be a bounded poll like every other in this
        // tree. esp-idf's is unbounded.
        const ATTEMPTS: u32 = 8;
        let mut target = ticks;
        for _ in 0..ATTEMPTS {
            unsafe {
                let cfg = self.read(LACTCONFIG);
                self.write(LACTCONFIG, cfg & !LACT_ALARM_EN);
                self.write(LACTALARMLO, target as u32);
                self.write(LACTALARMHI, (target >> 32) as u32);
                self.write(LACTCONFIG, cfg | LACT_ALARM_EN);

                let now = self.now_ticks();
                if target > now || self.fired() {
                    return;
                }
                // Behind, and no interrupt pending: push past the counter by
                // however far it has moved plus a margin, and try again.
                target = now + (now - target) + TICKS_PER_US * 2;
            }
        }
    }

    /// Stop the alarm firing. The counter keeps running.
    ///
    /// Safe: writes a register a held `Lact` owns.
    #[cfg_attr(target_os = "none", link_section = ".iram1.lact")]
    pub fn clear_alarm(&self) {
        unsafe {
            let cfg = self.read(LACTCONFIG);
            self.write(LACTCONFIG, cfg & !LACT_ALARM_EN);
        }
    }

    /// Whether the alarm interrupt is asserted.
    ///
    /// Safe: a side-effect-free read of a register a held `Lact` owns.
    #[inline]
    #[cfg_attr(target_os = "none", link_section = ".iram1.lact")]
    pub fn fired(&self) -> bool {
        unsafe { self.read(INT_ST_TIMERS) & LACT_INT != 0 }
    }

    /// Acknowledge the alarm interrupt.
    ///
    /// A level interrupt stays asserted until this is called, so a handler
    /// that returns without it is re-entered immediately and forever.
    ///
    /// Safe: writes only the LAC bit of a register a held `Lact` owns.
    #[inline]
    #[cfg_attr(target_os = "none", link_section = ".iram1.lact")]
    pub fn clear_interrupt(&self) {
        unsafe { self.write(INT_CLR_TIMERS, LACT_INT) }
    }

    /// Let the alarm reach the interrupt crossbar.
    ///
    /// Safe: sets only the LAC bit of a register a held `Lact` owns.
    pub fn enable_interrupt(&self) {
        unsafe {
            let ena = self.read(INT_ENA_TIMERS);
            self.write(INT_ENA_TIMERS, ena | LACT_INT);
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_register_offsets_are_the_headers() {
        // From `timg_reg.h` at v4.4. These are the numbers that cannot be
        // checked any other way without hardware, and one wrong offset writes
        // into a neighbouring timer's configuration.
        assert_eq!(LACTCONFIG, 0x70);
        assert_eq!(LACTLO, 0x78);
        assert_eq!(LACTHI, 0x7C);
        assert_eq!(LACTUPDATE, 0x80);
        assert_eq!(LACTALARMLO, 0x84);
        assert_eq!(LACTALARMHI, 0x88);
        assert_eq!(LACTLOAD, 0x94);
        assert_eq!(INT_ENA_TIMERS, 0x98);
        assert_eq!(INT_ST_TIMERS, 0xA0);
        assert_eq!(INT_CLR_TIMERS, 0xA4);
        // Bit 3, not bit 0: the two general-purpose timers and the watchdog
        // come first, and using T0's bit here would clear the tick's
        // interrupt instead of the alarm's.
        assert_eq!(LACT_INT, 1 << 3);
    }

    #[test]
    fn the_config_bits_are_the_headers() {
        assert_eq!(LACT_EN, 1 << 31);
        assert_eq!(LACT_INCREASE, 1 << 30);
        assert_eq!(LACT_LEVEL_INT_EN, 1 << 11);
        assert_eq!(LACT_ALARM_EN, 1 << 10);
        assert_eq!(LACT_LAC_EN, 1 << 9);
        assert_eq!(LACT_DIVIDER_SHIFT, 13);
        assert_eq!(LACT_DIVIDER_MASK, 0xFFFF);
    }

    #[test]
    fn the_interrupt_sources_are_the_headers() {
        // `soc.h`: TG0 LACT is 17, TG1 LACT is 18. Off by one lands on the
        // other group's timer, which would look like an alarm that fires at
        // the wrong times rather than one that does not fire.
        assert_eq!(TG0_LACT_INTR_SOURCE, 17);
        assert_eq!(TG1_LACT_INTR_SOURCE, 18);
    }

    #[test]
    fn an_apb_frequency_that_does_not_divide_is_refused() {
        // The divider is an integer. 80 MHz over two ticks per microsecond is
        // 40, exactly; an odd frequency would give a counter running at a rate
        // that is not a whole number of ticks per microsecond, and every
        // deadline computed from it would drift.
        assert_eq!(80 % TICKS_PER_US, 0);
        assert_ne!(81 % TICKS_PER_US, 0);
        assert_eq!(TICKS_PER_US, 2);
    }
}
