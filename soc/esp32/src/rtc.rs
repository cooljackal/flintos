// SPDX-License-Identifier: Apache-2.0

//! The RTC_CNTL register block.
//!
//! RTC_CNTL is a peripheral several unrelated things need: the CPU-frequency
//! measurement wants its counter, the second core is stalled through it, the
//! reset reason is read from it, one watchdog lives in it, and the radio asks
//! it for the crystal frequency. Every one of those had its own copy of the
//! base address and its own offsets, and two of them had their own copy of the
//! latch-and-read sequence below.
//!
//! Chip infrastructure that more than one driver wants is exactly what a
//! `soc/*` crate is for, so it lives here once.
//!
//! # It is not on the CPU clock
//!
//! RTC_CNTL runs on the RTC slow clock — nominally 150 kHz from an internal RC
//! oscillator, against a CPU at 80 or 240 MHz. **A write does not take effect
//! when the store retires.** That is not a detail: `appcpu::stall` returned
//! immediately after requesting a stall, the caller disabled the other core's
//! cache in the microseconds before the request landed, and the core executed
//! rubbish. Anything here that changes state has to say how the caller knows
//! it happened.
//!
//! Register facts from esp-idf `components/soc/esp32/register/soc/
//! rtc_cntl_reg.h`, cross-checked against NuttX where the encoding is not
//! obvious.

use crate::addr::RTC_CNTL_BASE;

// ── Offsets ─────────────────────────────────────────────────────────────────
//
// Named rather than spelled at each use, because a wrong one here is silent:
// the read returns a plausible number from a neighbouring register.

/// `RTC_CNTL_OPTIONS0_REG`. Holds `SW_STALL_APPCPU_C0` in bits [1:0].
pub const OPTIONS0: u32 = RTC_CNTL_BASE;
/// `RTC_CNTL_TIME_UPDATE_REG`. Bit 31 requests a sample, bit 30 says it landed.
pub const TIME_UPDATE: u32 = RTC_CNTL_BASE + 0x0C;
/// `RTC_CNTL_TIME0_REG` — the low 32 bits of the counter.
pub const TIME0: u32 = RTC_CNTL_BASE + 0x10;
/// `RTC_CNTL_TIME1_REG` — the high bits of the counter.
pub const TIME1: u32 = RTC_CNTL_BASE + 0x14;
/// `RTC_CNTL_RESET_STATE_REG` — why each core last reset.
pub const RESET_STATE: u32 = RTC_CNTL_BASE + 0x34;
/// `RTC_CNTL_WDTCONFIG0_REG` — the RTC watchdog's control register.
pub const WDTCONFIG0: u32 = RTC_CNTL_BASE + 0x8C;
/// `RTC_CNTL_WDTCONFIG1_REG` — stage 0 timeout, in RTC slow-clock ticks.
pub const WDTCONFIG1: u32 = RTC_CNTL_BASE + 0x90;
/// `RTC_CNTL_WDTFEED_REG`.
pub const WDTFEED: u32 = RTC_CNTL_BASE + 0xA0;
/// `RTC_CNTL_WDTWPROTECT_REG` — the write-protect key register.
pub const WDTWPROTECT: u32 = RTC_CNTL_BASE + 0xA4;
/// `RTC_CNTL_SW_CPU_STALL_REG`. Holds `SW_STALL_APPCPU_C1` in bits [25:20].
pub const SW_CPU_STALL: u32 = RTC_CNTL_BASE + 0xAC;
/// `RTC_XTAL_FREQ_REG`, which is `RTC_CNTL_STORE4_REG`.
///
/// A retention register the bootloader writes, not a hardware one — nothing
/// in silicon measures the crystal and puts it here.
pub const XTAL_FREQ: u32 = RTC_CNTL_BASE + 0xB0;

/// `RTC_CNTL_TIME_UPDATE`, bit 31: write to request a counter sample.
const TIME_UPDATE_REQ: u32 = 1 << 31;
/// `RTC_CNTL_TIME_VALID`, bit 30: reads set once the sample has landed.
const TIME_VALID: u32 = 1 << 30;

// ── The counter ─────────────────────────────────────────────────────────────

/// Sample the 48-bit RTC counter.
///
/// The counter is latched rather than read live: ask with `TIME_UPDATE`, wait
/// for `TIME_VALID`, then read the halves. Reading them without the latch can
/// straddle a carry and produce a time that never happened.
///
/// Returns `None` if the sample has not landed after `max_polls` reads, so a
/// stopped or missing RTC block fails rather than hanging the caller. Both
/// callers care: one runs during boot, the other inside PHY initialisation,
/// and a hang in either points nowhere near the clock that stopped.
///
/// esp-idf's `rtc_time_get` follows this with a write to `RTC_CNTL_INT_CLR_REG`
/// to drop a stale "time valid" interrupt flag. FlintOS never enables that
/// interrupt, so there is nothing to clear.
///
/// # Safety
/// Reads and writes RTC_CNTL registers. No side effects beyond the sample
/// request, which is what the register is for.
pub unsafe fn counter(max_polls: u32) -> Option<u64> {
    unsafe {
        (TIME_UPDATE as *mut u32).write_volatile(TIME_UPDATE_REQ);
        let mut polls = 0u32;
        while (TIME_UPDATE as *const u32).read_volatile() & TIME_VALID == 0 {
            polls += 1;
            if polls > max_polls {
                return None;
            }
        }
        let lo = (TIME0 as *const u32).read_volatile() as u64;
        let hi = (TIME1 as *const u32).read_volatile() as u64;
        Some((hi << 32) | lo)
    }
}

// ── Clocks ──────────────────────────────────────────────────────────────────

/// Nominal RTC slow-clock rate, used as the reference when measuring the CPU
/// frequency.
///
/// The ESP32 defaults `RTC_SLOW_CLK` to an internal 150 kHz RC oscillator --
/// untrimmed, commonly 5-10% off -- unless something has switched it to the
/// external 32 kHz crystal or the 8 MHz/256 divider. FlintOS's boot path does
/// neither. The imprecision is fine for the only question being asked: "80,
/// 160 or 240 MHz?", which a clock good to 10% resolves easily.
pub const SLOW_HZ_NOMINAL: u64 = 150_000;

/// CPU frequencies the second-stage bootloader could plausibly have left the
/// part running at.
pub const PLAUSIBLE_CPU_HZ: [u32; 3] = [80_000_000, 160_000_000, 240_000_000];

/// What to assume when the measurement fails, in Hz.
///
/// 80 MHz, because that is what the stock esp-idf second-stage bootloader --
/// the one espflash writes -- configures before handing off. It expects the
/// *application* to raise the clock during its own init, which FlintOS does
/// not do. 240 MHz is the figure an esp-idf application sees only after
/// calling `esp_clk_init()`.
///
/// A caller falling back to this should say so out loud: every timeout in the
/// system is scaled by it, and using a wrong clock silently is what caused
/// issue #6 in the first place.
pub const DEFAULT_CPU_HZ: u32 = 80_000_000;

/// Snap a raw frequency measurement to the nearest plausible ESP32 CPU clock,
/// or `None` if it is more than 25% from any of them.
///
/// Rounding rather than reporting the raw number is the point: the RTC
/// reference is an RC oscillator, so the raw figure is never exactly right,
/// and a tick period computed from it would drift. Refusing a measurement
/// that lands nowhere near a real clock is what keeps a stopped or
/// misconfigured reference from being believed.
pub fn round_to_plausible(raw_hz: u64) -> Option<u32> {
    let mut best = PLAUSIBLE_CPU_HZ[0] as u64;
    let mut best_diff = raw_hz.abs_diff(best);
    for &candidate in &PLAUSIBLE_CPU_HZ[1..] {
        let diff = raw_hz.abs_diff(candidate as u64);
        if diff < best_diff {
            best_diff = diff;
            best = candidate as u64;
        }
    }
    if best_diff * 4 > best {
        None
    } else {
        Some(best as u32)
    }
}

// ── The crystal ─────────────────────────────────────────────────────────────

/// Crystals the ESP32 supports, in MHz.
pub const XTAL_40_MHZ: u32 = 40;
pub const XTAL_26_MHZ: u32 = 26;

/// The crystal frequency in MHz as the bootloader recorded it, or `None` if
/// the register does not hold a credible one.
///
/// The encoding is not obvious and is not guessed. NuttX documents it:
///
/// > Values of RTC_XTAL_FREQ_REG and RTC_APB_FREQ_REG are stored as two
/// > copies in lower and upper 16-bit halves.
///
/// and esp-idf's `rtc_clk_xtal_freq_get` reads the same register at the same
/// offset. Two sources agreeing is what makes this safe to write.
///
/// The duplicated halves are the validity check: a register nothing has
/// written reads as something whose halves differ. esp-idf also keeps a
/// "ROM log disabled" flag in bit 16 — bit 0 of the upper half — and
/// compensates by setting bit 0 of the value, so the halves still match and
/// the low bit may be set spuriously. Hence the mask.
///
/// **Returning `None` rather than a default is deliberate.** A wrong crystal
/// mis-calibrates the radio, and the symptom is poor range rather than
/// anything pointing here, so the caller is made to decide what to do about
/// not knowing.
///
/// # Safety
/// Reads one RTC_CNTL register. No side effects.
pub unsafe fn xtal_freq_mhz() -> Option<u32> {
    let reg = unsafe { (XTAL_FREQ as *const u32).read_volatile() };
    let lo = reg & 0xFFFF;
    let hi = (reg >> 16) & 0xFFFF;
    if lo == 0 || lo != hi {
        return None;
    }
    match lo & !1 {
        XTAL_40_MHZ => Some(XTAL_40_MHZ),
        XTAL_26_MHZ => Some(XTAL_26_MHZ),
        _ => None,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_match_rtc_cntl_reg_h() {
        // Transcribed from esp-idf's header. Pinned here because every one of
        // these was previously spelled out in a different crate, and a wrong
        // offset reads a neighbouring register rather than failing.
        assert_eq!(OPTIONS0, 0x3FF4_8000);
        assert_eq!(TIME_UPDATE, 0x3FF4_800C);
        assert_eq!(TIME0, 0x3FF4_8010);
        assert_eq!(TIME1, 0x3FF4_8014);
        assert_eq!(RESET_STATE, 0x3FF4_8034);
        assert_eq!(WDTCONFIG0, 0x3FF4_808C);
        assert_eq!(WDTCONFIG1, 0x3FF4_8090);
        assert_eq!(WDTFEED, 0x3FF4_80A0);
        assert_eq!(WDTWPROTECT, 0x3FF4_80A4);
        assert_eq!(SW_CPU_STALL, 0x3FF4_80AC);
        assert_eq!(XTAL_FREQ, 0x3FF4_80B0);
    }

    #[test]
    fn every_offset_is_distinct() {
        // The failure a copy-paste produces: two names, one register, and
        // whichever is written last wins silently.
        let all = [
            OPTIONS0, TIME_UPDATE, TIME0, TIME1, RESET_STATE, WDTCONFIG0,
            WDTCONFIG1, WDTFEED, WDTWPROTECT, SW_CPU_STALL, XTAL_FREQ,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "two names for one register");
            }
        }
    }

    #[test]
    fn the_counter_latch_bits_are_the_documented_ones() {
        // Bit 31 requests, bit 30 reports. Swapping them gives a wait that
        // never ends on a perfectly healthy chip.
        assert_eq!(TIME_UPDATE_REQ, 0x8000_0000);
        assert_eq!(TIME_VALID, 0x4000_0000);
        assert_ne!(TIME_UPDATE_REQ, TIME_VALID);
    }

    #[test]
    fn a_measurement_snaps_to_a_real_clock_or_is_refused() {
        // Within 25% of a candidate: snap to it. The RC reference is 5-10%
        // off, so exact numbers never arrive.
        assert_eq!(round_to_plausible(80_000_000), Some(80_000_000));
        assert_eq!(round_to_plausible(76_000_000), Some(80_000_000));
        assert_eq!(round_to_plausible(232_000_000), Some(240_000_000));
        assert_eq!(round_to_plausible(158_000_000), Some(160_000_000));

        // Nowhere near one: refused rather than rounded to the nearest, which
        // is how a stopped reference would otherwise be believed.
        assert_eq!(round_to_plausible(0), None);
        assert_eq!(round_to_plausible(1_000), None);
        assert_eq!(round_to_plausible(2_000_000_000), None);
    }

    #[test]
    fn the_fallback_is_what_the_bootloader_leaves_not_what_idf_reports() {
        // 240 MHz is what an esp-idf app sees *after* esp_clk_init(). FlintOS
        // never calls it, so assuming 240 would scale every timeout by 3.
        assert_eq!(DEFAULT_CPU_HZ, 80_000_000);
        assert!(PLAUSIBLE_CPU_HZ.contains(&DEFAULT_CPU_HZ));
    }

    #[test]
    fn only_crystals_the_esp32_has_are_accepted() {
        // The register is a retention word, so anything at all can be in it.
        // The check that matters is the duplicated halves; these pin the
        // decode around it.
        assert_eq!(XTAL_40_MHZ, 40);
        assert_eq!(XTAL_26_MHZ, 26);

        // The decode, without the hardware read: halves must match, bit 0 is
        // esp-idf's ROM-log flag and is masked off.
        fn decode(reg: u32) -> Option<u32> {
            let (lo, hi) = (reg & 0xFFFF, (reg >> 16) & 0xFFFF);
            if lo == 0 || lo != hi {
                return None;
            }
            match lo & !1 {
                40 => Some(40),
                26 => Some(26),
                _ => None,
            }
        }
        assert_eq!(decode(0x0028_0028), Some(40));
        assert_eq!(decode(0x001A_001A), Some(26));
        assert_eq!(decode(0x0029_0029), Some(40), "the ROM-log flag is masked");
        assert_eq!(decode(0x0000_0000), None, "never written");
        assert_eq!(decode(0x0028_0000), None, "halves disagree");
        assert_eq!(decode(0x0018_0018), None, "24 MHz is not an ESP32 crystal");
    }
}
