// SPDX-License-Identifier: Apache-2.0

//! Low-power sleep: the RTC_CNTL sleep FSM, timer wake only.
//!
//! Two modes, both woken by the RTC timer:
//!
//! - **Light sleep** pauses the CPU and gates its clock; SRAM and every
//!   register keep their contents, so execution resumes at the instruction
//!   after the one that asked to sleep. The kernel tick (Xtensa CCOUNT) stops
//!   with the CPU, so the caller must reconcile elapsed time on wake — see
//!   [`elapsed_us_from_ticks`] and the kernel's `power` module.
//! - **Deep sleep** powers the digital domain down. Only the RTC still has
//!   power, so nothing in SRAM survives and the timer wake arrives as a chip
//!   reset into the bootloader, not a return from this function.
//!
//! # This is a first, safe cut
//!
//! esp-idf's `esp_light_sleep_start` does a great deal more than the register
//! sequence here: it suspends the flash, drops the PLL, switches the CPU to
//! the RTC clock, and powers down the RF and memory domains for the lowest
//! possible current. Every one of those is a way to hang a board that cannot
//! wake, and none is needed for *correctness* — with the domains left powered
//! the FSM still pauses the CPU and still wakes on the timer, it just draws
//! more current than a tuned sleep would. This cut takes reliable wake over
//! minimum current; that power-domain tuning is a documented follow-up.
//!
//! What it does *not* skip is `light_init` — the power-up/wait counters and
//! analog bias of `rtc_sleep_init` / `rtc_sleep_low_init`. Those are not a
//! current optimisation: without them the FSM gates the CPU and the wake
//! transition never completes, a silent hang (this was the #31 bug).
//!
//! # Wake sources
//!
//! Timer only. RTC-GPIO, touch and ULP wake are separate pad/analog paths
//! (`esp_sleep_enable_ext1_wakeup`, `..._touchpad_wakeup`, `..._ulp_wakeup`)
//! and are deferred. A caller that needs them must not rely on this module.
//!
//! # Register facts
//!
//! The sequence follows esp-idf exactly, cited per step:
//!
//! - entry/poll: `components/esp_hw_support/port/esp32/rtc_sleep.c`
//!   (`rtc_sleep_start`)
//! - timer arming: `components/esp_hw_support/sleep_modes.c`
//!   (`timer_wakeup_prepare`) and
//!   `components/hal/esp32/include/hal/rtc_cntl_ll.h`
//!   (`rtc_cntl_ll_set_wakeup_timer`)
//! - deep-sleep domain power-down: `rtc_sleep_init`, the `cfg.deep_slp` branch
//! - offsets/fields: `components/soc/esp32/register/soc/rtc_cntl_reg.h`

use crate::addr::{RTC_CNTL_BASE, UART0_BASE};
use crate::poll;
use crate::reg;
use crate::rtc;

// ── Register offsets (relative to RTC_CNTL_BASE) ─────────────────────────────
//
// Transcribed from rtc_cntl_reg.h. A wrong one here is silent: it pokes a
// neighbouring RTC register, and the failure surfaces as a sleep that never
// wakes — the worst outcome this module has.

/// `RTC_CNTL_SLP_TIMER0_REG` — low 32 bits of the wake-time comparator.
const SLP_TIMER0: u32 = RTC_CNTL_BASE + 0x04;
/// `RTC_CNTL_SLP_TIMER1_REG` — high 16 bits of the wake-time comparator.
const SLP_TIMER1: u32 = RTC_CNTL_BASE + 0x08;
/// `RTC_CNTL_STATE0_REG` — holds `SLEEP_EN`, the FSM start bit.
const STATE0: u32 = RTC_CNTL_BASE + 0x18;
/// `RTC_CNTL_OPTIONS0_REG` — holds `XTL_FORCE_PU`, keep the crystal alive.
const OPTIONS0: u32 = RTC_CNTL_BASE;
/// `RTC_CNTL_TIMER1_REG` — PLL/XTAL/CK8M wake-buffer wait times.
const TIMER1: u32 = RTC_CNTL_BASE + 0x1C;
/// `RTC_CNTL_TIMER3_REG` — ROM/RAM and WiFi power-up/wait counters.
const TIMER3: u32 = RTC_CNTL_BASE + 0x24;
/// `RTC_CNTL_TIMER4_REG` — RTC-peri and digital-wrap power-up/wait counters.
const TIMER4: u32 = RTC_CNTL_BASE + 0x28;
/// `RTC_CNTL_TIMER5_REG` — holds `MIN_SLP_VAL` and the RTC-mem counters.
const TIMER5: u32 = RTC_CNTL_BASE + 0x2C;
/// `RTC_CNTL_BIAS_CONF_REG` — holds `DBG_ATTEN`, zeroed for light sleep.
const BIAS_CONF: u32 = RTC_CNTL_BASE + 0x78;
/// `RTC_CNTL_WAKEUP_STATE_REG` — holds the `WAKEUP_ENA` source mask.
const WAKEUP_STATE: u32 = RTC_CNTL_BASE + 0x38;
/// `RTC_CNTL_INT_RAW_REG` — the FSM's wake/reject result flags.
const INT_RAW: u32 = RTC_CNTL_BASE + 0x40;
/// `RTC_CNTL_INT_CLR_REG` — write-1-to-clear for the flags above.
const INT_CLR: u32 = RTC_CNTL_BASE + 0x48;
/// `RTC_CNTL_SLP_REJECT_CONF_REG` — which sources may reject the sleep.
const SLP_REJECT_CONF: u32 = RTC_CNTL_BASE + 0x64;
/// `RTC_CNTL_DIG_PWC_REG` — digital-domain power control (deep sleep).
const DIG_PWC: u32 = RTC_CNTL_BASE + 0x84;
/// `RTC_CNTL_PWC_REG` — RTC fast/slow memory and peripheral power control.
const PWC: u32 = RTC_CNTL_BASE + 0x80;
/// `RTC_CNTL_SDIO_CONF_REG` — the VDDSDIO regulator's sleep control.
const SDIO_CONF: u32 = RTC_CNTL_BASE + 0x74;
/// `RTC_CNTL_CLK_CONF_REG` — the RTC clock sources, incl. the 8 MHz oscillator.
const CLK_CONF: u32 = RTC_CNTL_BASE + 0x70;
/// `RTC_CNTL_REG` — the analog bias register, holding the sleep/wake DBIAS
/// fields for the digital and RTC domains.
const RTC_REG: u32 = RTC_CNTL_BASE + 0x7C;

// ── Register fields ──────────────────────────────────────────────────────────

/// `RTC_CNTL_SLEEP_EN`, bit 31 of `STATE0`: start the sleep FSM.
const SLEEP_EN: u32 = 1 << 31;

/// `RTC_CNTL_WAKEUP_ENA` field: mask `0x7FF` at bit 11 of `WAKEUP_STATE`.
const WAKEUP_ENA_MASK: u32 = 0x7FF << 11;
const WAKEUP_ENA_SHIFT: u32 = 11;

/// `RTC_TIMER_TRIG_EN` (`soc/rtc.h`): bit 3 of the wake-source mask selects the
/// RTC timer. Written into the `WAKEUP_ENA` field, so shifted up by 11.
const TIMER_TRIG_EN: u32 = 1 << 3;

/// `RTC_CNTL_MIN_SLP_VAL` field: mask `0xFF` at bit 8 of `TIMER5`.
const MIN_SLP_VAL_MASK: u32 = 0xFF << 8;
const MIN_SLP_VAL_SHIFT: u32 = 8;
/// `RTC_CNTL_MIN_SLP_VAL_MIN` (`rtc.h`): the shortest sleep the FSM accepts,
/// in slow-clock ticks. Set so a 20 ms self-test sleep is not rejected as too
/// short.
const MIN_SLP_VAL_MIN: u32 = 2;

/// `RTC_CNTL_SLP_WAKEUP_INT_RAW` / `_CLR`, bit 0: the sleep ended on a wake.
const SLP_WAKEUP_INT: u32 = 1 << 0;
/// `RTC_CNTL_SLP_REJECT_INT_RAW` / `_CLR`, bit 1: the FSM refused to sleep.
const SLP_REJECT_INT: u32 = 1 << 1;

/// `RTC_CNTL_DG_WRAP_PD_EN`, bit 31 of `DIG_PWC`: power the digital core down
/// during sleep. This is the single bit that makes a sleep *deep* — wake then
/// arrives as a reset rather than a return.
const DG_WRAP_PD_EN: u32 = 1 << 31;

// ── Wake-transition timer fields (rtc_sleep_init) ────────────────────────────
//
// The FSM that pauses the CPU for light sleep needs the power-up and wait
// counters for every domain programmed before it will complete the *wake*
// transition. Left at reset (all zero) the FSM gates the CPU and never brings
// it back — a silent hang, the exact failure this module hit on hardware
// (#31). Each `(mask, value)` pair below is one `REG_SET_FIELD` from esp-idf's
// `rtc_sleep_init`; `_S` are the field shifts and `_V` the field widths from
// `rtc_cntl_reg.h`. Every power-up/wait counter is
// `RTC_CNTL_OTHER_BLOCKS_*_CYCLES` = 1.

/// `TIMER3`: ROM/RAM and WiFi power-up (`_S 25/9`, `_V 0x7F`) and wait
/// (`_S 16/0`, `_V 0x1FF`) counters. `TIMER4` uses the identical layout for
/// its RTC-peri and digital-wrap counters, so the same pair serves both.
const TIMER34_MASK: u32 = (0x7F << 25) | (0x1FF << 16) | (0x7F << 9) | 0x1FF;
const TIMER34_VAL: u32 = (1 << 25) | (1 << 16) | (1 << 9) | 1;

/// `TIMER5`: `MIN_SLP_VAL` (`_S 8`, `_V 0xFF`) = the short-sleep floor, plus
/// the RTC-mem power-up (`_S 25`, `_V 0x7F`) and wait (`_S 16`, `_V 0x1FF`).
const TIMER5_MASK: u32 = MIN_SLP_VAL_MASK | (0x7F << 25) | (0x1FF << 16);
const TIMER5_VAL: u32 = (MIN_SLP_VAL_MIN << MIN_SLP_VAL_SHIFT) | (1 << 25) | (1 << 16);

/// `TIMER1` (`rtc_sleep_low_init`): PLL-buf wait (`_S 24`, `_V 0xFF`) = 1 and
/// CK8M wait (`_S 6`, `_V 0xFF`) = 4 cycles. The XTAL-buf wait (`_S 14`,
/// `_V 0x3FF`) is a µs figure converted to ticks, so it is filled in at run
/// time from [`us_to_slowclk`] rather than baked in here.
const TIMER1_MASK: u32 = (0xFF << 24) | (0x3FF << 14) | (0xFF << 6);
const TIMER1_FIXED_VAL: u32 = (1 << 24) | (4 << 6);
/// `RTC_CNTL_XTL_BUF_WAIT_SLP_US`: the XTAL settle time to convert to ticks.
const XTL_BUF_WAIT_US: u64 = 500;
const XTL_BUF_WAIT_SHIFT: u32 = 14;

/// `RTC_CNTL_XTL_FORCE_PU`, bit 13 of `OPTIONS0`: keep the crystal powered
/// through the sleep so wake does not wait for it to restart (`cfg.xtal_fpu`).
const XTL_FORCE_PU: u32 = 1 << 13;

/// `RTC_CNTL_DBG_ATTEN` field (`_S 24`, `_V 0x3`) of `BIAS_CONF`, zeroed on the
/// non-deep path.
const DBG_ATTEN_MASK: u32 = 0x3 << 24;
/// `RTC_CNTL_DBG_ATTEN_DEFAULT` (= 3), the value esp-idf's `rtc_sleep_finish`
/// restores on every wake. `light_init` zeroes the field for the sleep; leaving
/// it there under-biases the RTC regulator, which rev-3 silicon tolerates but
/// rev-1 does not — the core browns out at 240 MHz on wake and the console
/// garbles into a reset loop.
const DBG_ATTEN_DEFAULT: u32 = 0x3 << 24;

// ── The rest of rtc_sleep_init (memory/domain power, VDDSDIO, bias) ──────────
//
// esp-idf's `rtc_sleep_init` programs far more than the wake-transition timers
// above: it keeps the RTC fast/slow memory powered and un-isolated, hands
// VDDSDIO back to the state machine, and sets the sleep/wake analog bias. On a
// rev-3 die a sleep woke without any of it; a rev-1 die gates the CPU and never
// wakes, so the full config is ported here. Values are esp-idf's light-sleep
// defaults (`RTC_SLEEP_CONFIG_DEFAULT` with no power-down flags), bit positions
// from `rtc_cntl_reg.h`.

/// `RTC_CNTL_PWC_REG`: keep both RTC memories powered, un-isolated, and not
/// following the CPU through the sleep — `FASTMEM/SLOWMEM_FORCE_PU|_FORCE_NOISO`
/// set, `_PD_EN|_FOLW_CPU` clear — plus `PD_EN` (the RTC-peripheral power-down)
/// clear. The memories the wake logic sits beside stay alive.
const PWC_SET: u32 = (1 << 13) | (1 << 16) | (1 << 0) | (1 << 2); // FASTMEM/SLOWMEM FORCE_PU|FORCE_NOISO
const PWC_CLEAR: u32 = (1 << 14) | (1 << 17) | (1 << 6) | (1 << 9) | (1 << 20); // *_PD_EN, *_FOLW_CPU, peri PD_EN

/// `RTC_CNTL_DIG_PWC_REG`: no Wi-Fi power-down, no low-sleep-memory force-up.
const DIG_PWC_CLEAR: u32 = (1 << 30) | (1 << 4); // WIFI_PD_EN | LSLP_MEM_FORCE_PU

/// `RTC_CNTL_CK8M_FORCE_PU`, bit 26 of `CLK_CONF`: cleared — the slow clock is
/// the 150 kHz RC, not the 8 MHz/256 divider, so the 8 MHz oscillator need not
/// be forced up.
const CK8M_FORCE_PU: u32 = 1 << 26;

/// `RTC_CNTL_SDIO_CONF_REG`: return VDDSDIO to state-machine control
/// (`SDIO_FORCE` clear) and do not power it down (`SDIO_PD_EN` clear).
const SDIO_CONF_CLEAR: u32 = (1 << 22) | (1 << 21); // SDIO_FORCE | SDIO_PD_EN

/// `RTC_CNTL_REG` analog bias, in the three fields it is safe to set here:
/// `RTC_DBIAS_WAK` (`_S 25`), `RTC_DBIAS_SLP` (`_S 22`) and `DIG_DBIAS_SLP`
/// (`_S 8`), all to `RTC_CNTL_DBIAS_1V10` (level 4).
///
/// `DIG_DBIAS_WAK` (`_S 11`) is **deliberately excluded**: it is the digital
/// core's bias while awake, and esp-idf's 1.10 V would brown out a core running
/// at 240 MHz. esp-idf can use it because it drops the CPU clock before sleeping
/// and raises it after; FlintOS sleeps straight from 240 MHz, so
/// `cpu_clk::set_240mhz` owns that field and holds it at 1.25 V (level 7). The
/// RTC-domain bias is independent of the CPU clock, so setting it is safe and
/// is what the wake comparator's analog runs on.
const DBIAS_MASK: u32 = (0x7 << 25) | (0x7 << 22) | (0x7 << 8);
const DBIAS_1V10: u32 = 4;
const DBIAS_VAL: u32 = (DBIAS_1V10 << 25) | (DBIAS_1V10 << 22) | (DBIAS_1V10 << 8);

// ── Slow-clock ↔ microsecond conversion ──────────────────────────────────────
//
// The wake comparator counts RTC slow-clock ticks, the caller thinks in
// microseconds, so both directions are needed. Pure arithmetic, split out and
// host-tested, because getting the scale wrong is a sleep of the wrong length
// and — for a too-short value below MIN_SLP_VAL — a rejected sleep.
//
// # The reference is nominal, not calibrated
//
// esp-idf calibrates the slow clock against the crystal each boot
// (`rtc_clk_cal`) because the internal RC oscillator is 5–10% off. This cut
// uses the nominal 150 kHz ([`rtc::SLOW_HZ_NOMINAL`]) instead, so a requested
// 20 ms sleep may really be 18–22 ms. That is fine here precisely because the
// caller does not trust the *requested* duration: it measures the *actual*
// elapsed slow-clock ticks across the sleep and reconciles the tick from
// those, so the RC oscillator's error cancels out. Calibrating the reference
// is a follow-up that lowers the per-sleep error, not a correctness fix.

/// Microseconds to slow-clock ticks, rounded down.
///
/// Saturates rather than overflows: a `u64` of microseconds times 150 000
/// would wrap past ~123 000 years, which no real sleep reaches, but a
/// saturating multiply keeps a bad caller from arming a garbage comparator.
pub fn us_to_slowclk(us: u64) -> u64 {
    us.saturating_mul(rtc::SLOW_HZ_NOMINAL) / 1_000_000
}

/// Slow-clock ticks to microseconds, rounded down. The inverse of
/// [`us_to_slowclk`], used to turn a measured tick delta into elapsed time.
pub fn elapsed_us_from_ticks(ticks: u64) -> u64 {
    ticks.saturating_mul(1_000_000) / rtc::SLOW_HZ_NOMINAL
}

// ── The sleep primitives ─────────────────────────────────────────────────────

/// A sleep could not be entered or measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepError {
    /// The RTC counter never latched a sample, so the wake time could not be
    /// computed. The RTC slow clock is stopped or missing.
    NoClock,
    /// The FSM did not report wake or reject within the poll bound. On real
    /// silicon a *rejected* sleep returns here having never paused the CPU; a
    /// sleep that paused but never woke cannot reach this — the CPU is halted —
    /// which is the hazard the short, always-timer-armed duration guards
    /// against.
    Timeout,
    /// The FSM rejected the sleep (a wake condition was already pending). The
    /// CPU never paused; no time was lost.
    Rejected,
}

/// Poll bound for the wake/reject flags.
///
/// Deliberately large. During a successful light sleep the CPU is halted, so
/// this loop is not spinning and the count does not accrue — it only bounds
/// the pathological case where the FSM neither sleeps nor rejects, which must
/// fail as a timeout rather than wedge the caller.
const SLEEP_POLL_SPINS: u32 = 5_000_000;

/// Arm the RTC timer to wake `sleep_us` microseconds from now.
///
/// The comparator is absolute: esp-idf's `timer_wakeup_prepare` writes
/// `rtc_ticks_at_sleep_start + duration_ticks`, so this samples the counter,
/// adds the duration, and writes both halves via the sequence in
/// `rtc_cntl_ll_set_wakeup_timer`.
///
/// Returns the counter value it sampled, so the caller can measure the true
/// elapsed ticks after wake. `NoClock` if the counter never latched.
///
/// # Safety
/// Writes RTC_CNTL sleep registers.
unsafe fn arm_timer(sleep_us: u64) -> Result<u64, SleepError> {
    let now = unsafe { rtc::counter(poll::DEFAULT_SPINS) }.ok_or(SleepError::NoClock)?;
    let wake_at = now.wrapping_add(us_to_slowclk(sleep_us));
    unsafe {
        // rtc_cntl_ll_set_wakeup_timer: low half then high half.
        reg::write(reg::at(SLP_TIMER0, 0), (wake_at & 0xFFFF_FFFF) as u32);
        reg::write(reg::at(SLP_TIMER1, 0), (wake_at >> 32) as u32);
    }
    Ok(now)
}

/// Wait for the console UART (UART0) to finish transmitting.
///
/// Light sleep gates the APB clock that times the UART, and the wake path
/// rebuilds it at a different rate for a moment — so any byte still in UART0's
/// TX FIFO or shift register when the clock moves is transmitted at the wrong
/// baud and arrives as garbage. esp-idf flushes the console UART before sleep
/// for the same reason. This drains the FIFO (`TXFIFO_CNT`, bits 23:16) and
/// then waits for the shift register to empty (`ST_UTX_OUT`, bits 27:24, zero
/// when idle), so nothing is in flight across the clock change.
///
/// # Safety
/// Reads a UART0 status register.
unsafe fn drain_console() {
    /// `UART_STATUS_REG` — TX FIFO count and transmit state.
    const UART0_STATUS: u32 = UART0_BASE + 0x1C;
    const TXFIFO_CNT: u32 = 0xFF << 16;
    const ST_UTX_OUT: u32 = 0xF << 24;
    unsafe {
        let _ = poll::until(
            || reg::read(reg::at(UART0_STATUS, 0)) & (TXFIFO_CNT | ST_UTX_OUT) == 0,
            poll::DEFAULT_SPINS,
        );
    }
}

/// Program the power-domain state machine for a light sleep, `sleep_flags = 0`.
///
/// This is the light-sleep path of esp-idf's `rtc_sleep_init` followed by
/// `rtc_sleep_low_init`, and it is the step whose omission hung the board on
/// #31: without the power-up/wait counters the wake transition never completes
/// and the gated CPU never resumes. It leaves every domain powered (no
/// `*_pd_en`), keeps the crystal up, and sets the analog bias to 1.10 V, so the
/// FSM pauses the CPU and reliably wakes it — trading current for a wake that
/// always returns, matching this module's stance.
///
/// # Safety
/// Writes RTC_CNTL power-control registers. Must run before [`enter`].
unsafe fn light_init() {
    unsafe {
        // rtc_sleep_init: power-up/wait counters for every domain (the fix).
        reg::modify(reg::at(TIMER3, 0), TIMER34_MASK, TIMER34_VAL);
        reg::modify(reg::at(TIMER4, 0), TIMER34_MASK, TIMER34_VAL);
        reg::modify(reg::at(TIMER5, 0), TIMER5_MASK, TIMER5_VAL);

        // Keep the RTC fast/slow memories powered and un-isolated, hand VDDSDIO
        // back to the state machine, and leave the Wi-Fi domain up — the rest of
        // rtc_sleep_init for a no-power-down light sleep. rev-1 silicon does not
        // wake without this; rev-3 did.
        reg::set(reg::at(PWC, 0), PWC_SET);
        reg::clear(reg::at(PWC, 0), PWC_CLEAR);
        reg::clear(reg::at(DIG_PWC, 0), DIG_PWC_CLEAR);
        reg::clear(reg::at(CLK_CONF, 0), CK8M_FORCE_PU);
        reg::clear(reg::at(SDIO_CONF, 0), SDIO_CONF_CLEAR);
        reg::modify(reg::at(RTC_REG, 0), DBIAS_MASK, DBIAS_VAL);

        // Non-deep branch: keep the digital core powered, no debug attenuation.
        reg::clear(reg::at(DIG_PWC, 0), DG_WRAP_PD_EN);
        reg::modify(reg::at(BIAS_CONF, 0), DBG_ATTEN_MASK, 0);

        // xtal_fpu = 1: hold the crystal so wake is fast, not a cold restart.
        reg::set(reg::at(OPTIONS0, 0), XTL_FORCE_PU);

        // rtc_sleep_low_init: PLL/XTAL/CK8M settle times. XTAL wait is a µs
        // figure in slow-clock ticks; the rest are fixed cycle counts.
        let xtl_wait = (us_to_slowclk(XTL_BUF_WAIT_US) as u32) << XTL_BUF_WAIT_SHIFT;
        reg::modify(reg::at(TIMER1, 0), TIMER1_MASK, TIMER1_FIXED_VAL | xtl_wait);
    }
}

/// Enter the FSM and wait for it to report wake or reject.
///
/// This is `rtc_sleep_start`: set the wake-source mask, set the reject config,
/// clear the stale flags, set `SLEEP_EN`, then poll `INT_RAW` for either
/// result. On a light sleep the CPU halts inside the poll and resumes when the
/// timer fires; on a deep sleep it never returns.
///
/// # Safety
/// Starts the sleep FSM. The caller must have armed a wake source, or the CPU
/// halts with nothing to wake it.
unsafe fn enter() -> Result<(), SleepError> {
    unsafe {
        // rtc_sleep_start: REG_SET_FIELD(WAKEUP_STATE, WAKEUP_ENA, wakeup_opt).
        reg::modify(
            reg::at(WAKEUP_STATE, 0),
            WAKEUP_ENA_MASK,
            (TIMER_TRIG_EN << WAKEUP_ENA_SHIFT) & WAKEUP_ENA_MASK,
        );
        // reject_opt = wakeup_opt for a timer-only sleep (sleep_modes.c:417).
        reg::write(reg::at(SLP_REJECT_CONF, 0), TIMER_TRIG_EN);
        // Clear any stale wake/reject flag before starting.
        reg::write(reg::at(INT_CLR, 0), SLP_WAKEUP_INT | SLP_REJECT_INT);
        // Start the FSM.
        reg::set(reg::at(STATE0, 0), SLEEP_EN);

        // Wait for wake or reject. `spin_loop` inside `poll::until`.
        poll::until(
            || reg::read(reg::at(INT_RAW, 0)) & (SLP_WAKEUP_INT | SLP_REJECT_INT) != 0,
            SLEEP_POLL_SPINS,
        )
        .map_err(|_| SleepError::Timeout)?;

        let raw = reg::read(reg::at(INT_RAW, 0));
        if raw & SLP_REJECT_INT != 0 && raw & SLP_WAKEUP_INT == 0 {
            Err(SleepError::Rejected)
        } else {
            Ok(())
        }
    }
}

/// Light sleep for approximately `sleep_us` microseconds, woken by the RTC
/// timer.
///
/// Returns the **measured** elapsed time in microseconds — the slow-clock tick
/// delta across the sleep, converted back — not the requested duration. The
/// kernel feeds this to its tick reconciliation so the scheduler's clock
/// accounts for the paused interval. On a rejected sleep the return is roughly
/// zero (the CPU never paused).
///
/// State is retained: on return, execution continues here and every register
/// and all of SRAM hold what they did before.
///
/// # Safety
/// Pauses this CPU. Must be called with a sane RTC clock; the timer wake is
/// what guarantees return, so callers must not disable it. Do not call from
/// the second core — this arms one shared FSM.
pub unsafe fn light_sleep(sleep_us: u64) -> Result<u64, SleepError> {
    unsafe {
        let start = arm_timer(sleep_us)?;
        light_init();
        // Flush the console before the clock moves, or bytes mid-flight garble.
        drain_console();
        match enter() {
            Ok(()) => {
                // rtc_sleep_finish: restore DBG_ATTEN to its default. light_init
                // zeroed it for the sleep, and set_240mhz does not touch the
                // BIAS_CONF register, so without this the RTC regulator stays
                // under-biased on wake — harmless on rev-3 silicon, a brownout
                // at 240 MHz on rev-1. (esp-idf esp32/rtc_sleep.c.)
                reg::modify(reg::at(BIAS_CONF, 0), DBG_ATTEN_MASK, DBG_ATTEN_DEFAULT);

                // Wake resumes on the 40 MHz crystal with the BBPLL powered
                // down — the FSM does not restore the clock tree, so every
                // clock derived from the PLL (the CPU, and the APB the UART is
                // timed from) is wrong until software rebuilds it. Re-run the
                // boot-time raise, which reads the current source, brings the
                // 480 MHz PLL back, points the CPU at it, and restores the
                // 1.25 V digital bias. Interrupts are masked by the caller, so
                // this runs single-core and quiet, as the routine requires.
                crate::cpu_clk::set_240mhz();
                let end = rtc::counter(poll::DEFAULT_SPINS).ok_or(SleepError::NoClock)?;
                Ok(elapsed_us_from_ticks(end.wrapping_sub(start)))
            }
            Err(SleepError::Rejected) => Ok(0),
            Err(e) => Err(e),
        }
    }
}

/// Deep sleep until the RTC timer fires, which arrives as a **chip reset**.
///
/// This does not return on success: powering the digital domain down loses the
/// CPU and all of SRAM, so the timer wake restarts the chip through the
/// bootloader exactly as a power-on would. Only RTC memory survives (this cut
/// stores nothing there — a follow-up). A caller reaches the line after this
/// only if the FSM rejected the sleep or timed out.
///
/// The single bit that distinguishes this from [`light_sleep`] is
/// `DG_WRAP_PD_EN` (rtc_sleep_init, `cfg.deep_slp`); the aggressive analog and
/// RF power-down esp-idf also does is deferred with the rest of the tuning.
///
/// # Safety
/// Powers the CPU down. Everything in SRAM is lost. As [`light_sleep`], the
/// timer wake must stay armed.
pub unsafe fn deep_sleep(sleep_us: u64) -> Result<(), SleepError> {
    unsafe {
        arm_timer(sleep_us)?;
        // rtc_sleep_init cfg.deep_slp: power the digital core down so wake is a
        // reset. Set last, so an error in arming the timer above leaves the
        // chip in a normal (returnable) state.
        reg::set(reg::at(DIG_PWC, 0), DG_WRAP_PD_EN);
        match enter() {
            Ok(()) => Ok(()), // unreachable on hardware: the chip has reset
            Err(e) => {
                // Undo the power-down so the caller is not left primed to lose
                // state on some later, unrelated sleep.
                reg::clear(reg::at(DIG_PWC, 0), DG_WRAP_PD_EN);
                Err(e)
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_match_rtc_cntl_reg_h() {
        // Transcribed from esp-idf's header; a wrong one pokes a neighbour.
        assert_eq!(SLP_TIMER0, 0x3FF4_8004);
        assert_eq!(SLP_TIMER1, 0x3FF4_8008);
        assert_eq!(STATE0, 0x3FF4_8018);
        assert_eq!(TIMER5, 0x3FF4_802C);
        assert_eq!(WAKEUP_STATE, 0x3FF4_8038);
        assert_eq!(INT_RAW, 0x3FF4_8040);
        assert_eq!(INT_CLR, 0x3FF4_8048);
        assert_eq!(SLP_REJECT_CONF, 0x3FF4_8064);
        assert_eq!(DIG_PWC, 0x3FF4_8084);
        // light_init's registers.
        assert_eq!(OPTIONS0, 0x3FF4_8000);
        assert_eq!(TIMER1, 0x3FF4_801C);
        assert_eq!(TIMER3, 0x3FF4_8024);
        assert_eq!(TIMER4, 0x3FF4_8028);
        assert_eq!(BIAS_CONF, 0x3FF4_8078);
    }

    #[test]
    fn light_init_field_values_match_rtc_sleep_init() {
        // Every power-up/wait counter is OTHER_BLOCKS_*_CYCLES = 1; the shared
        // TIMER3/TIMER4 layout packs four of them.
        assert_eq!(TIMER34_VAL, (1 << 25) | (1 << 16) | (1 << 9) | 1);
        // The four fields tile the whole register (7+9+7+9 = 32 bits).
        assert_eq!(TIMER34_MASK, 0xFFFF_FFFF);
        // TIMER5 carries MIN_SLP_VAL = 2 alongside the RTC-mem counters.
        assert_eq!(TIMER5_VAL, (2 << 8) | (1 << 25) | (1 << 16));
        // XTL settle: 500 us at 150 kHz = 75 ticks, into the field at bit 14.
        assert_eq!(us_to_slowclk(XTL_BUF_WAIT_US), 75);
        assert_eq!((us_to_slowclk(XTL_BUF_WAIT_US) as u32) << XTL_BUF_WAIT_SHIFT, 75 << 14);
        // TIMER1 fixed cycles: PLL_BUF_WAIT = 1 (bit 24), CK8M_WAIT = 4 (bit 6).
        assert_eq!(TIMER1_FIXED_VAL, (1 << 24) | (4 << 6));
        // XTL_FORCE_PU is bit 13; DBG_ATTEN a 2-bit field at bit 24.
        assert_eq!(XTL_FORCE_PU, 1 << 13);
        assert_eq!(DBG_ATTEN_MASK, 0x3 << 24);
        // rtc_sleep_finish restores DBG_ATTEN to RTC_CNTL_DBG_ATTEN_DEFAULT = 3.
        assert_eq!(DBG_ATTEN_DEFAULT, 3 << 24);
    }

    #[test]
    fn fields_are_the_documented_bits() {
        assert_eq!(SLEEP_EN, 0x8000_0000);
        assert_eq!(DG_WRAP_PD_EN, 0x8000_0000);
        // WAKEUP_ENA is an 11-bit field at bit 11.
        assert_eq!(WAKEUP_ENA_MASK, 0x003F_F800);
        // The timer source lands at bit 11+3 = 14 once shifted into the field.
        assert_eq!(TIMER_TRIG_EN << WAKEUP_ENA_SHIFT, 1 << 14);
        // MIN_SLP_VAL is an 8-bit field at bit 8.
        assert_eq!(MIN_SLP_VAL_MASK, 0x0000_FF00);
        assert_eq!(SLP_WAKEUP_INT, 1);
        assert_eq!(SLP_REJECT_INT, 2);
    }

    #[test]
    fn microseconds_round_trip_through_slow_clock() {
        // 20 ms at 150 kHz is 3000 ticks; back again is 20 000 us exactly.
        assert_eq!(us_to_slowclk(20_000), 3_000);
        assert_eq!(elapsed_us_from_ticks(3_000), 20_000);
        // 1 ms -> 150 ticks -> 1 ms.
        assert_eq!(us_to_slowclk(1_000), 150);
        assert_eq!(elapsed_us_from_ticks(150), 1_000);
    }

    #[test]
    fn a_short_sleep_clears_the_min_slp_val_floor() {
        // The self-test sleeps 20 ms = 3000 ticks, far above the 2-tick floor,
        // so it is never rejected as too short. A 1 us sleep would be 0 ticks,
        // which is why the floor and a sane minimum duration both matter.
        assert!(us_to_slowclk(20_000) > MIN_SLP_VAL_MIN as u64);
        assert_eq!(us_to_slowclk(1), 0, "a 1 us request rounds to nothing");
    }

    #[test]
    fn conversion_saturates_rather_than_wrapping() {
        // A preposterous duration must not wrap into a tiny tick count and arm
        // a comparator that has already passed.
        assert_eq!(us_to_slowclk(u64::MAX), u64::MAX / 1_000_000);
    }
}
