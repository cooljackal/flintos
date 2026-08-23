// SPDX-License-Identifier: Apache-2.0

//! MCPWM: motor-control PWM — one unit (MCPWM0), timer 0, operator 0.
//!
//! This is the peripheral LEDC is not: a complementary output pair with
//! hardware **dead-time** insertion, a **fault** input that forces the outputs
//! low in silicon, and a **capture** unit for timing an input edge. Dead time
//! is why MCPWM, not LEDC, drives an H-bridge — it stops both transistors of a
//! leg conducting at once, a short that destroys the bridge, and it has to be
//! enforced by the hardware because software cannot be trusted to be that
//! prompt.
//!
//! # What this cut covers
//!
//! Timer 0 up-counting at a configured frequency; operator 0 generating an
//! active-high pulse on PWM0A whose width is a compare value; the dead-time
//! module deriving PWM0B as A's complement with configurable rising- and
//! falling-edge delays; fault F0 as a latched (one-shot) trip forcing both
//! outputs low; and capture channel 0 timestamping edges. One unit and one
//! operator — MCPWM1 and operators 1/2 are the same registers at a fixed
//! stride and are a mechanical extension, not new hardware understanding.
//!
//! # Register facts
//!
//! `DR_REG_MCPWM0_BASE` = `0x3FF5E000` (esp-idf `soc/soc.h`, `DR_REG_PWM0_BASE`).
//! Fields from `soc/mcpwm_reg.h` / `mcpwm_struct.h`; sequences from
//! `hal/esp32/include/hal/mcpwm_ll.h` and `driver/mcpwm.c`. Clock/reset is
//! DPORT bit 17 ([`dport::ClockBit::PWM0`]).
//!
//! | Register | Offset | Fields |
//! |---|---|---|
//! | `CLK_CFG` | `0x00` | `CLK_PRESCALE` `[7:0]` |
//! | `TIMER0_CFG0` | `0x04` | `PRESCALE` `[7:0]`, `PERIOD` `[23:8]` |
//! | `TIMER0_CFG1` | `0x08` | `START` `[2:0]`, `MOD` `[4:3]` |
//! | `OPERATOR_TIMERSEL` | `0x38` | `OP0_SEL` `[1:0]` |
//! | `GEN0_TSTMP_A` | `0x40` | compare A `[15:0]` |
//! | `GEN0_A` | `0x50` | `UTEZ` `[1:0]`, `UTEA` `[5:4]` (up: zero/compareA actions) |
//! | `DT0_CFG` | `0x58` | dead-time mode bits (active-high complementary) |
//! | `DT0_FED_CFG`/`DT0_RED_CFG` | `0x5C`/`0x60` | falling/rising edge delay `[15:0]` |
//! | `FH0_CFG0` | `0x68` | `F0_OST` 7, A/B one-shot forced-low actions |
//! | `FH0_CFG1` | `0x6C` | `CLR_OST` 0 (rising edge re-arms) |
//! | `FH0_STATUS` | `0x70` | `OST_ON` 1 |
//! | `FAULT_DETECT` | `0xE4` | `F0_EN` 0, `F0_POLE` 3 |
//! | `CAP_TIMER_CFG` | `0xE8` | `TIMER_EN` 0 |
//! | `CAP_CH0_CFG` | `0xF0` | `EN` 0, `MODE` `[2:1]`, `PRESCALE` `[10:3]` |
//! | `CAP_CH0` | `0xFC` | 32-bit timestamp |
//! | `CAP_STATUS` | `0x108` | `CAP0_EDGE` 0 (1 = negedge) |
//! | `INT_RAW`/`INT_CLR` | `0x114`/`0x11C` | `CAP0` bit 27 |

#![no_std]
// Bit-position clarity: fields at bit 0 are written `x << 0` to match the field
// table above, deliberately, rather than collapsing the shift away.
#![allow(clippy::identity_op)]

use hal::bus::{BusError, BusResult};
use hal::pinmux::PinPull;
use soc_esp32::{dport, gpio_matrix, io_mux, reg};

/// MCPWM0 register block.
const MCPWM0_BASE: u32 = 0x3FF5_E000;

// Register offsets from the unit base.
const CLK_CFG: u32 = 0x00;
const TIMER0_CFG0: u32 = 0x04;
const TIMER0_CFG1: u32 = 0x08;
const OPERATOR_TIMERSEL: u32 = 0x38;
const GEN0_TSTMP_A: u32 = 0x40;
const GEN0_A: u32 = 0x50;
const GEN0_B: u32 = 0x54;
const DT0_CFG: u32 = 0x58;
const DT0_FED_CFG: u32 = 0x5C;
const DT0_RED_CFG: u32 = 0x60;
const FH0_CFG0: u32 = 0x68;
const FH0_CFG1: u32 = 0x6C;
const FH0_STATUS: u32 = 0x70;
const FAULT_DETECT: u32 = 0xE4;
const CAP_TIMER_CFG: u32 = 0xE8;
const CAP_CH0_CFG: u32 = 0xF0;
const CAP_CH0: u32 = 0xFC;
const CAP_STATUS: u32 = 0x108;
const INT_RAW: u32 = 0x114;
const INT_CLR: u32 = 0x11C;

// TIMER0_CFG0 fields.
const TIMER_PRESCALE_SHIFT: u32 = 0;
const TIMER_PERIOD_SHIFT: u32 = 8;

// TIMER0_CFG1: timer_mod=1 (up), timer_start=2 (run continuously).
const TIMER_RUN_UP: u32 = (2 << 0) | (1 << 3);
const TIMER_FREEZE: u32 = 0;

/// `GEN0_A`: set A high at timer==zero (`UTEZ`=2), force A low at
/// timer==compareA (`UTEA`=1). Action encoding 0=none,1=low,2=high,3=toggle.
const GEN_A_COMPLEMENTARY: u32 = (2 << 0) | (1 << 4);

/// `DT0_CFG` = active-high complementary: only `FED_OUTINVERT` (bit 14) set, and
/// crucially both `A_OUTBYPASS` (15) and `B_OUTBYPASS` (16) cleared — they reset
/// to 1, so the whole word must be written, never OR-ed. A takes the source
/// through the rising-edge delay, B takes the inverted source through the
/// falling-edge delay: an active-high complementary pair with dead time.
const DT_ACTIVE_HIGH_COMPLEMENTARY: u32 = 1 << 14;

// FAULT_DETECT.
const F0_EN: u32 = 1 << 0;
const F0_POLE: u32 = 1 << 3;

/// `FH0_CFG0`: enable F0 one-shot (`bit 7`) and force both A and B low on trip
/// in both count directions. Forced-action fields are 2 bits, value 1 = low:
/// A-ost-down `[13:12]`, A-ost-up `[15:14]`, B-ost-down `[21:20]`, B-ost-up `[23:22]`.
const FH0_F0_OST: u32 = 1 << 7;
const FH0_FORCE_AB_LOW: u32 = (1 << 12) | (1 << 14) | (1 << 20) | (1 << 22);

const FH_CLR_OST: u32 = 1 << 0;
const FH_OST_ON: u32 = 1 << 1;

// Capture.
const CAP_TIMER_EN: u32 = 1 << 0;
const CAP_EN: u32 = 1 << 0;
const CAP_MODE_NEG: u32 = 1 << 1;
const CAP_MODE_POS: u32 = 1 << 2;
const CAP0_INT: u32 = 1 << 27;
const CAP0_EDGE_NEG: u32 = 1 << 0; // in CAP_STATUS: 1 = the captured edge was falling

// GPIO-matrix signal indices (esp-idf `gpio_sig_map.h`).
const PWM0A_OUT_IDX: u32 = 32;
const PWM0B_OUT_IDX: u32 = 33;
const PWM0_F0_IN_IDX: u32 = 34;
const PWM0_CAP0_IN_IDX: u32 = 109;

/// The PWM clock is 160 MHz divided by (`clk_prescale` + 1). Both the timer
/// (through its own prescaler) and the dead-time/capture blocks derive from it.
pub const PWM_CLK_HZ: u32 = 160_000_000;

/// One up-counting complementary configuration.
///
/// Output frequency = `PWM_CLK_HZ / (clk_prescale+1) / (timer_prescale+1) /
/// (period+1)`. A is high for `compare_a` timer ticks each period, so the duty
/// is `compare_a / (period+1)` before dead time. `dead_red`/`dead_fed` are in
/// PWM-clock cycles (`PWM_CLK_HZ / (clk_prescale+1)`): `dead_red` delays A's
/// rising edge, `dead_fed` delays B's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub clk_prescale: u16,
    pub timer_prescale: u16,
    pub period: u16,
    pub compare_a: u16,
    pub dead_red: u16,
    pub dead_fed: u16,
}

/// A captured edge: the capture-timer timestamp and whether it was a falling
/// edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capture {
    pub timestamp: u32,
    pub falling: bool,
}

/// The MCPWM0 controller.
pub struct Mcpwm {
    base: u32,
}

impl Mcpwm {
    /// Bring MCPWM0 up: clock it, release its reset, and set the unit clock
    /// prescaler from `cfg`.
    ///
    /// # Safety
    /// Takes exclusive ownership of the MCPWM0 registers and DPORT bit 17.
    pub unsafe fn new(cfg: &Config) -> Self {
        dport::enable(dport::ClockBit::PWM0);
        let m = Mcpwm { base: MCPWM0_BASE };
        unsafe {
            m.w(CLK_CFG, cfg.clk_prescale as u32);
            m.configure(cfg);
        }
        m
    }

    unsafe fn r(&self, off: u32) -> u32 {
        reg::read((self.base + off) as *mut u32)
    }
    unsafe fn w(&self, off: u32, val: u32) {
        reg::write((self.base + off) as *mut u32, val);
    }

    /// Program timer 0, operator 0, the generator, and the dead-time module for
    /// a complementary pair. Does not start the timer — see [`start`](Self::start).
    unsafe fn configure(&self, cfg: &Config) {
        unsafe {
            // Timer 0: up-count, prescaler + period. Written stopped (mod stays
            // frozen until `start`).
            self.w(
                TIMER0_CFG0,
                ((cfg.timer_prescale as u32) << TIMER_PRESCALE_SHIFT)
                    | ((cfg.period as u32) << TIMER_PERIOD_SHIFT),
            );
            self.w(TIMER0_CFG1, TIMER_FREEZE);

            // Operator 0 runs off timer 0; duty compare A; generator makes A a
            // high-from-zero, low-at-compareA pulse. Generator B stays 0 — B is
            // synthesised by the dead-time block, not its own generator.
            self.w(OPERATOR_TIMERSEL, 0);
            self.w(GEN0_TSTMP_A, cfg.compare_a as u32);
            self.w(GEN0_A, GEN_A_COMPLEMENTARY);
            self.w(GEN0_B, 0);

            // Dead time: edge delays, then the active-high complementary mode.
            self.w(DT0_RED_CFG, cfg.dead_red as u32);
            self.w(DT0_FED_CFG, cfg.dead_fed as u32);
            self.w(DT0_CFG, DT_ACTIVE_HIGH_COMPLEMENTARY);
        }
    }

    /// Start (or restart) timer 0 counting up. Outputs begin toggling.
    ///
    /// # Safety
    /// Drives the outputs; the pads must be routed first.
    pub unsafe fn start(&self) {
        unsafe { self.w(TIMER0_CFG1, TIMER_RUN_UP) };
    }

    /// Freeze timer 0. The last output levels are held.
    ///
    /// # Safety
    /// Writes the timer register.
    pub unsafe fn stop(&self) {
        unsafe { self.w(TIMER0_CFG1, TIMER_FREEZE) };
    }

    /// Route PWM0A / PWM0B to output pads. The pad's input buffer is left on so
    /// the same pad can be read back through the matrix (by PCNT or capture) for
    /// a wireless self-test.
    ///
    /// # Safety
    /// Writes the pad's IO_MUX and the matrix output register.
    pub unsafe fn route_output_a(&self, pin: u8) -> BusResult<()> {
        unsafe { route_out(pin, PWM0A_OUT_IDX) }
    }
    /// See [`route_output_a`](Self::route_output_a).
    ///
    /// # Safety
    /// As [`route_output_a`](Self::route_output_a).
    pub unsafe fn route_output_b(&self, pin: u8) -> BusResult<()> {
        unsafe { route_out(pin, PWM0B_OUT_IDX) }
    }

    /// Read PWM0A / PWM0B from a pad into the fault or capture input — used with
    /// a pad already driven elsewhere.
    ///
    /// # Safety
    /// Writes the pad's IO_MUX and the matrix input register.
    pub unsafe fn route_fault_input(&self, pin: u8) -> BusResult<()> {
        unsafe { route_in(pin, PWM0_F0_IN_IDX) }
    }
    /// See [`route_fault_input`](Self::route_fault_input).
    ///
    /// # Safety
    /// As [`route_fault_input`](Self::route_fault_input).
    pub unsafe fn route_capture_input(&self, pin: u8) -> BusResult<()> {
        unsafe { route_in(pin, PWM0_CAP0_IN_IDX) }
    }

    /// Enable fault F0 as a latched (one-shot) trip that forces both outputs low
    /// in hardware. `active_high` selects the input level that trips.
    ///
    /// # Safety
    /// Writes the fault registers.
    pub unsafe fn enable_fault_oneshot(&self, active_high: bool) {
        unsafe {
            self.w(FH0_CFG0, FH0_F0_OST | FH0_FORCE_AB_LOW);
            self.w(FAULT_DETECT, F0_EN | if active_high { F0_POLE } else { 0 });
        }
    }

    /// Whether the one-shot fault is currently latched (outputs forced low).
    pub fn fault_tripped(&self) -> bool {
        unsafe { self.r(FH0_STATUS) & FH_OST_ON != 0 }
    }

    /// Clear a latched one-shot fault. The clear is edge-triggered, so this
    /// pulses the bit low then high. The fault source must already be inactive,
    /// or the trip latches again immediately.
    ///
    /// # Safety
    /// Writes the fault-handler register.
    pub unsafe fn clear_fault(&self) {
        unsafe {
            self.w(FH0_CFG1, 0);
            self.w(FH0_CFG1, FH_CLR_OST);
        }
    }

    /// Start the capture timer and arm capture channel 0 on both edges, with no
    /// input prescale. The capture timer is clocked from the 80 MHz APB (not the
    /// 160 MHz PWM clock), so a timestamp difference is in 12.5 ns ticks.
    ///
    /// # Safety
    /// Writes the capture registers.
    pub unsafe fn enable_capture_both_edges(&self) {
        unsafe {
            self.w(CAP_TIMER_CFG, CAP_TIMER_EN);
            self.w(CAP_CH0_CFG, CAP_EN | CAP_MODE_POS | CAP_MODE_NEG);
            self.w(INT_CLR, CAP0_INT);
        }
    }

    /// Poll for the next captured edge, non-blocking. Returns `None` until an
    /// edge has been timestamped, then the timestamp and its polarity, clearing
    /// the event so the next call waits for the following edge.
    ///
    /// # Safety
    /// Reads and clears the capture registers.
    pub unsafe fn poll_capture(&self) -> Option<Capture> {
        unsafe {
            if self.r(INT_RAW) & CAP0_INT == 0 {
                return None;
            }
            let timestamp = self.r(CAP_CH0);
            let falling = self.r(CAP_STATUS) & CAP0_EDGE_NEG != 0;
            self.w(INT_CLR, CAP0_INT);
            Some(Capture { timestamp, falling })
        }
    }
}

/// Route a peripheral output to `pin`, leaving the input buffer on so the pad
/// can be tapped back through the matrix.
unsafe fn route_out(pin: u8, idx: u32) -> BusResult<()> {
    if io_mux::offset(pin).is_none() {
        return Err(BusError::InvalidConfig);
    }
    unsafe {
        io_mux::configure(pin, io_mux::gpio_function(pin), true, PinPull::None)?;
        gpio_matrix::connect_output(pin, idx, true, false)
    }
}

/// Route `pin` into a peripheral input.
unsafe fn route_in(pin: u8, idx: u32) -> BusResult<()> {
    if io_mux::offset(pin).is_none() {
        return Err(BusError::InvalidConfig);
    }
    unsafe {
        io_mux::configure(pin, io_mux::gpio_function(pin), true, PinPull::None)?;
        gpio_matrix::connect_input(idx, pin, false)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_offsets_match_the_struct() {
        assert_eq!(MCPWM0_BASE, 0x3FF5_E000);
        assert_eq!(TIMER0_CFG0, 0x04);
        assert_eq!(GEN0_A, 0x50);
        assert_eq!(DT0_CFG, 0x58);
        assert_eq!(DT0_FED_CFG, 0x5C);
        assert_eq!(DT0_RED_CFG, 0x60);
        assert_eq!(FH0_CFG0, 0x68);
        assert_eq!(FAULT_DETECT, 0xE4);
        assert_eq!(CAP_CH0, 0xFC);
        assert_eq!(CAP_STATUS, 0x108);
        assert_eq!(INT_RAW, 0x114);
    }

    #[test]
    fn generator_makes_an_active_high_pulse() {
        // A high at zero (UTEZ=high=2, bits `[1:0]`) and low at compareA
        // (UTEA=low=1, bits `[5:4]`).
        assert_eq!(GEN_A_COMPLEMENTARY, (2 << 0) | (1 << 4));
        assert_eq!(GEN_A_COMPLEMENTARY, 0x12);
    }

    #[test]
    fn dead_time_mode_only_inverts_the_falling_edge_output() {
        // Active-high complementary is FED_OUTINVERT (bit 14) and nothing else —
        // in particular the two bypass bits (15, 16) that reset to 1 are 0 here.
        assert_eq!(DT_ACTIVE_HIGH_COMPLEMENTARY, 1 << 14);
        assert_eq!(DT_ACTIVE_HIGH_COMPLEMENTARY & ((1 << 15) | (1 << 16)), 0);
    }

    #[test]
    fn the_fault_forces_both_outputs_low_in_both_directions() {
        // One-shot enable for F0, plus force-low (action 1) on A and B for the
        // up and down halves: A `[15:14]`/`[13:12]`, B `[23:22]`/`[21:20]`.
        assert_eq!(FH0_F0_OST, 1 << 7);
        assert_eq!(FH0_FORCE_AB_LOW, 0x0050_5000);
        assert_eq!(FH0_F0_OST | FH0_FORCE_AB_LOW, 0x0050_5080);
    }

    #[test]
    fn the_timer_runs_up_and_freezes() {
        // mod=1 (up) at `[4:3]`, start=2 (run) at `[2:0]`.
        assert_eq!(TIMER_RUN_UP, 0x0A);
        assert_eq!(TIMER_FREEZE, 0);
    }

    #[test]
    fn capture_and_signal_indices_are_the_documented_ones() {
        assert_eq!(PWM0A_OUT_IDX, 32);
        assert_eq!(PWM0B_OUT_IDX, 33);
        assert_eq!(PWM0_F0_IN_IDX, 34);
        assert_eq!(PWM0_CAP0_IN_IDX, 109);
        assert_eq!(CAP0_INT, 1 << 27);
        assert_eq!(CAP_MODE_POS | CAP_MODE_NEG, 0x6);
    }

    #[test]
    fn frequency_math_is_the_documented_formula() {
        // clk_prescale 0, timer_prescale 159, period 99 -> 160e6/1/160/100 = 10 kHz.
        let f = PWM_CLK_HZ / (0 + 1) / (159 + 1) / (99 + 1);
        assert_eq!(f, 10_000);
    }
}
