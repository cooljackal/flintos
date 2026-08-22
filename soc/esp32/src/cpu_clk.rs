// SPDX-License-Identifier: Apache-2.0

//! Raising the CPU clock from the 80 MHz boot default to 240 MHz.
//!
//! The stock second-stage bootloader espflash writes leaves the ESP32 at
//! 80 MHz and expects the *application* to raise the clock during its own init
//! (see [`crate::rtc::DEFAULT_CPU_HZ`]). FlintOS did not, so everything —
//! including the Wi-Fi blob, which Espressif builds and times against 240 MHz —
//! ran at a third of speed. This closes that: it is FlintOS's `esp_clk_init`.
//!
//! # A faithful port of esp-idf's `rtc_clk_cpu_freq_set_config`
//!
//! Going 80 → 240 MHz is not one register write. The 80 MHz clock runs the CPU
//! off a **320 MHz** BBPLL; 240 MHz needs a **480 MHz** BBPLL, and the PLL
//! cannot be retuned while the CPU is running from it. So the sequence, taken
//! from `components/esp_hw_support/port/esp32/rtc_clk.c` at v4.4, is: drop the
//! CPU to the 40 MHz crystal, power the 320 MHz PLL down, bring the 480 MHz PLL
//! up (analog `REGI2C` writes with the divisor constants for a 40 MHz crystal),
//! then point the CPU at it divided by two. Every constant here is transcribed
//! from that file; the values are the chip's, not ours to choose.
//!
//! The analog PLL writes go through the ROM's `rom_i2c_writeReg`, and the µs
//! delays through the ROM's `ets_delay_us` (paced by `g_ticks_per_us`, which
//! `ets_update_cpu_frequency` keeps in step with the CPU across the switch) —
//! the same primitives esp-idf uses, called at their fixed addresses the way
//! [`crate::appcpu`] calls the cache ROM routines.
//!
//! # APB is untouched
//!
//! The APB clock stays at 80 MHz on the ESP32 whatever the CPU runs at (see
//! [`crate::APB_HZ`]), so the UART console and the timer groups are unaffected
//! — only the CPU speeds up. The boot path measures the CPU frequency against
//! the RTC slow clock *after* this runs, so it reports 240 MHz on its own.

use crate::{dport, reg};

// ── ROM helpers, called at their fixed addresses (as `appcpu` does) ──────────

/// `void rom_i2c_writeReg(uint8_t block, uint8_t host_id, uint8_t reg, uint8_t data)`.
const ROM_I2C_WRITEREG: usize = 0x4000_41A4;
/// `void ets_delay_us(uint32_t us)`.
const ETS_DELAY_US: usize = 0x4000_8534;
/// `void ets_update_cpu_frequency(uint32_t mhz)` — updates `g_ticks_per_us`.
const ETS_UPDATE_CPU_FREQUENCY: usize = 0x4000_8550;

#[inline]
unsafe fn i2c_write(block: u8, host: u8, reg_add: u8, data: u8) {
    let f: extern "C" fn(u8, u8, u8, u8) = core::mem::transmute(ROM_I2C_WRITEREG);
    f(block, host, reg_add, data);
}
#[inline]
unsafe fn delay_us(us: u32) {
    let f: extern "C" fn(u32) = core::mem::transmute(ETS_DELAY_US);
    f(us);
}
#[inline]
unsafe fn update_cpu_freq(mhz: u32) {
    let f: extern "C" fn(u32) = core::mem::transmute(ETS_UPDATE_CPU_FREQUENCY);
    f(mhz);
}

// ── Registers (absolute; from soc/esp32 headers, see the porting spec) ───────

const RTC_CNTL_OPTIONS0: u32 = 0x3FF4_8000;
const RTC_CNTL_CLK_CONF: u32 = 0x3FF4_8070;
const RTC_CNTL_REG: u32 = 0x3FF4_807C;
const RTC_APB_FREQ_REG: u32 = 0x3FF4_80B4; // RTC_CNTL_STORE5
const DPORT_CPU_PER_CONF: u32 = 0x3FF0_003C;
const SYSCON_SYSCLK_CONF: u32 = 0x3FF6_6000;
const SYSCON_XTAL_TICK_CONF: u32 = 0x3FF6_6004;

// SOC_CLK_SEL field of RTC_CNTL_CLK_CONF: 2 bits at shift 27.
const SOC_CLK_SEL_MASK: u32 = 0x3 << 27;
const SOC_CLK_SEL_XTL: u32 = 0 << 27;
const SOC_CLK_SEL_PLL: u32 = 1 << 27;

// DIG_DBIAS_WAK field of RTC_CNTL_REG: 3 bits at shift 11. The digital core
// voltage. 240 MHz needs 1.25 V (level 7); the crystal is happy at 1.10 V (4).
// esp-idf trims 7 down by a per-chip efuse; 7 (the maximum) is the safe default
// — it can only over-volt, never under-volt, so it is stable on every part.
const DIG_DBIAS_MASK: u32 = 0x7 << 11;
const DIG_DBIAS_240M: u32 = 7 << 11;
const DIG_DBIAS_XTAL: u32 = 4 << 11;

// SYSCON_SYSCLK_CONF PRE_DIV_CNT: 10 bits at shift 0. Zero = divide by one.
const PRE_DIV_CNT_MASK: u32 = 0x3FF;

// DPORT_CPU_PER_CONF CPUPERIOD_SEL: 2 bits, value 2 selects 480/2 = 240 MHz.
// esp-idf writes the whole register, so the rest is zeroed with it.
const CPUPERIOD_SEL_240: u32 = 2;

// RTC_CNTL_OPTIONS0 BBPLL force-power-down bits: BB_I2C (6), BBPLL_I2C (8),
// BBPLL (10), and BIAS_I2C (18). Disable sets the first three; enable clears
// all four.
const BBPLL_PD_SET: u32 = (1 << 6) | (1 << 8) | (1 << 10);
const BBPLL_PD_CLEAR: u32 = (1 << 6) | (1 << 8) | (1 << 10) | (1 << 18);

// The BBPLL's REGI2C block and host id.
const I2C_BBPLL: u8 = 0x66;
const I2C_BBPLL_HOST: u8 = 4;

/// esp-idf waits exactly one RTC slow-clock cycle (via a TIMG0 calibration
/// busy-wait) after each domain-crossing write. One cycle of the 150 kHz RC
/// oscillator is ~6.7 µs; 40 µs is ~6 cycles — comfortably past the edge
/// without reaching for the calibration hardware. Boot-time and one-shot, so
/// the spare microseconds cost nothing.
const SLOW_CYCLE_US: u32 = 40;

/// Raise the CPU to 240 MHz.
///
/// Call once, early in boot, on the bootstrap core, before the frequency is
/// measured and before any second core exists — it is not synchronised.
///
/// # Safety
/// Reconfigures the clock tree and calls ROM routines. Must run single-core
/// with interrupts effectively quiet, before the radio (the other BBPLL user)
/// is brought up.
pub unsafe fn set_240mhz() {
    // Are we running off the PLL (the 80 MHz boot state) or already the crystal?
    let on_pll =
        ((RTC_CNTL_CLK_CONF as *const u32).read_volatile() & SOC_CLK_SEL_MASK) == SOC_CLK_SEL_PLL;

    // 1. Drop the CPU to the 40 MHz crystal so the BBPLL can be reconfigured.
    //    Update g_ticks_per_us first, so the delays below are paced correctly.
    update_cpu_freq(40);
    reg::modify(SYSCON_SYSCLK_CONF as *mut u32, PRE_DIV_CNT_MASK, 0); // divide by 1
    (SYSCON_XTAL_TICK_CONF as *mut u32).write_volatile(xtal_tick_conf(40)); // 40 MHz / 1 MHz - 1
    reg::modify(RTC_CNTL_CLK_CONF as *mut u32, SOC_CLK_SEL_MASK, SOC_CLK_SEL_XTL);
    apb_freq_update(40);
    reg::modify(RTC_CNTL_REG as *mut u32, DIG_DBIAS_MASK, DIG_DBIAS_XTAL);
    delay_us(SLOW_CYCLE_US);

    // 2. Power the (320 MHz) BBPLL down, if we were using it.
    if on_pll {
        reg::set(RTC_CNTL_OPTIONS0 as *mut u32, BBPLL_PD_SET);
    }

    // 3. Power the BBPLL back up and reset its configuration.
    reg::clear(RTC_CNTL_OPTIONS0 as *mut u32, BBPLL_PD_CLEAR);
    i2c_write(I2C_BBPLL, I2C_BBPLL_HOST, 0, 0x18); // IR_CAL_DELAY
    i2c_write(I2C_BBPLL, I2C_BBPLL_HOST, 1, 0x20); // IR_CAL_EXT_CAP
    i2c_write(I2C_BBPLL, I2C_BBPLL_HOST, 4, 0x9A); // OC_ENB_FCAL
    i2c_write(I2C_BBPLL, I2C_BBPLL_HOST, 10, 0x00); // OC_ENB_VCON
    i2c_write(I2C_BBPLL, I2C_BBPLL_HOST, 12, 0x00); // BBADC_CAL_7_0
    delay_us(SLOW_CYCLE_US);

    // 4. Configure the BBPLL to 480 MHz — the divisor constants for a 40 MHz
    //    crystal (div_ref 0, div7_0 28, div10_8 0, lref 0, dcur 6, bw 3).
    reg::modify(RTC_CNTL_REG as *mut u32, DIG_DBIAS_MASK, DIG_DBIAS_240M); // raise voltage
    delay_us(3); // DELAY_PLL_DBIAS_RAISE
    i2c_write(I2C_BBPLL, I2C_BBPLL_HOST, 11, 0xC3); // ENDIV5   (480 MHz)
    i2c_write(I2C_BBPLL, I2C_BBPLL_HOST, 9, 0x74); //  BBADC_DSMP (480 MHz)
    i2c_write(I2C_BBPLL, I2C_BBPLL_HOST, 2, 0x00); //  OC_LREF   = (lref<<7)|(div10_8<<4)|div_ref
    i2c_write(I2C_BBPLL, I2C_BBPLL_HOST, 3, 0x1C); //  OC_DIV_7_0 = div7_0 = 28
    i2c_write(I2C_BBPLL, I2C_BBPLL_HOST, 5, 0xC6); //  OC_DCUR   = (bw<<6)|dcur
    delay_us(80); // DELAY_PLL_ENABLE_WITH_150K (150 kHz slow clock)

    // 5. Point the CPU at the 480 MHz PLL, divided by two → 240 MHz.
    dport::write(DPORT_CPU_PER_CONF, CPUPERIOD_SEL_240);
    reg::modify(RTC_CNTL_REG as *mut u32, DIG_DBIAS_MASK, DIG_DBIAS_240M);
    reg::modify(RTC_CNTL_CLK_CONF as *mut u32, SOC_CLK_SEL_MASK, SOC_CLK_SEL_PLL);
    apb_freq_update(80); // APB is 80 MHz whatever the CPU runs at
    update_cpu_freq(240);
    delay_us(SLOW_CYCLE_US); // settle the switch before returning
}

/// Record the APB frequency where `rtc_clk_apb_freq_get` (and the blob) read
/// it: `RTC_APB_FREQ_REG`, holding `(hz >> 12)` duplicated into both 16-bit
/// halves — the encoding esp-idf's `clk_val_to_reg_val` produces.
#[inline]
unsafe fn apb_freq_update(mhz: u32) {
    (RTC_APB_FREQ_REG as *mut u32).write_volatile(apb_freq_reg_val(mhz * 1_000_000));
}

/// The value `esp_hw_support/port/esp32/rtc_clk.c` `rtc_clk_apb_freq_update`
/// stores in `RTC_APB_FREQ_REG`: `clk_val_to_reg_val(hz >> 12)`, i.e. the low
/// 16 bits of `hz >> 12` copied into both halves. Pure so the encoding can be
/// checked on the host, where a wrong shift or a missing duplication is silent.
const fn apb_freq_reg_val(hz: u32) -> u32 {
    let v = (hz >> 12) & 0xFFFF;
    v | (v << 16)
}

/// The `SYSCON_XTAL_TICK_CONF` value for a given crystal: one microsecond of
/// crystal ticks, minus one, so the APB µs reference divides cleanly. esp-idf
/// writes `xtal_freq_mhz - 1` (`rtc_clk.c`, `rtc_clk_cpu_freq_set_config`).
/// Pure so the off-by-one is testable off-target.
const fn xtal_tick_conf(xtal_mhz: u32) -> u32 {
    xtal_mhz - 1
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apb_reg_val_duplicates_the_shifted_halves() {
        // 80 MHz APB: 80_000_000 >> 12 = 19531 (0x4C4B), copied into both
        // halves. This is the number the Wi-Fi blob reads back for its timing,
        // so a wrong shift here mis-times the radio silently.
        assert_eq!(apb_freq_reg_val(80_000_000), 0x4C4B_4C4B);
        // 40 MHz APB (the crystal step mid-sequence).
        assert_eq!(apb_freq_reg_val(40_000_000), 0x2625_2625);
        // Both halves always match — the validity invariant every reader uses.
        for hz in [40_000_000u32, 80_000_000, 26_000_000] {
            let v = apb_freq_reg_val(hz);
            assert_eq!(v & 0xFFFF, v >> 16, "halves must match");
        }
    }

    #[test]
    fn xtal_tick_conf_is_mhz_minus_one() {
        // 40 MHz / 1 MHz - 1 = 39; a plain 40 would stretch the µs reference.
        assert_eq!(xtal_tick_conf(XTAL_40MHZ_TICK_INPUT), 39);
        assert_eq!(xtal_tick_conf(26), 25);
    }

    /// Documents the crystal the sequence is transcribed for.
    const XTAL_40MHZ_TICK_INPUT: u32 = 40;

    #[test]
    fn cpuperiod_selects_the_240_case() {
        // CPUPERIOD_SEL == 2 is the 480/2 = 240 MHz case in the TRM; 0 and 1
        // are 160 and are-you-sure territory.
        assert_eq!(CPUPERIOD_SEL_240, 2);
    }
}
