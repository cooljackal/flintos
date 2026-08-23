// SPDX-License-Identifier: Apache-2.0

//! PCNT: the ESP32's pulse counter — eight units, each a signed 16-bit counter
//! fed by an edge signal and steered by a control signal.
//!
//! What it is for: reading a quadrature encoder as a signed position with
//! direction, or counting a flow meter / tachometer. An edge on the *signal*
//! input steps the counter; the *control* input's level decides whether that
//! step is up or down. A per-unit **glitch filter** rejects pulses shorter than
//! a configured number of APB clock cycles, so contact bounce and line noise do
//! not show up as phantom counts.
//!
//! # The one non-obvious thing: mode is a matrix, not a direction
//!
//! There is no "count up" bit. Each channel has an action for the *positive*
//! edge and one for the *negative* edge (hold / increase / decrease), and then
//! the control level can *modify* that action: keep it, invert it, or hold. So
//! "up when the control pin is high, down when it is low" is expressed as
//! `pos = Increase`, control-high `= Keep`, control-low `= Inverse`. This is
//! exactly esp-idf's model (`pcnt_ll_set_edge_action` /
//! `pcnt_ll_set_level_action`), and getting it wrong reads as a counter that
//! moves the right amount in the wrong direction.
//!
//! # Register facts
//!
//! `DR_REG_PCNT_BASE` = `0x3FF57000` (esp-idf `soc/soc.h`). Layout from
//! `soc/pcnt_struct.h`; the field placement in `conf0` is the part a wrong map
//! silently corrupts, so it is pinned by the tests below. Each unit owns three
//! config words (`conf0/1/2`, 12 bytes) at `base + unit*0x0C`; the counters are
//! a separate block of eight words at `base + 0x60`; the global control /
//! reset / pause register is at `base + 0xB0`.
//!
//! The peripheral is clock-gated off out of reset (`DPORT_PCNT_CLK_EN`), like
//! every other ESP32 peripheral — [`PcntUnit::new`] enables it first, or every
//! register below reads back zero with no fault.

#![no_std]

use hal::bus::{BusError, BusResult};
use hal::pinmux::PinPull;
use soc_esp32::addr::PCNT_BASE;
use soc_esp32::{dport, gpio_matrix, io_mux, reg};

/// Number of PCNT units on this chip.
pub const NUM_UNITS: u8 = 8;

/// The widest glitch-filter threshold the hardware accepts (`filter_thres` is
/// ten bits; esp-idf's `PCNT_LL_MAX_GLITCH_WIDTH`).
pub const MAX_FILTER_THRES: u16 = 1023;

// ── Register offsets ────────────────────────────────────────────────────────

/// Bytes between one unit's config block and the next: `conf0`, `conf1`,
/// `conf2`.
const UNIT_CONF_STRIDE: u32 = 0x0C;
/// First counter register (`cnt_unit[0]`), one word per unit from here.
const CNT_BASE: u32 = 0x60;
/// The shared control register (`ctrl`): reset and pause bits, two per unit.
const CTRL: u32 = 0xB0;

// `conf0` field positions. From `soc/pcnt_struct.h`, unit 0's bitfield.
const CONF0_FILTER_THRES_MASK: u32 = 0x3FF; // [9:0]
const CONF0_FILTER_EN: u32 = 1 << 10;
const CONF0_CH0_NEG_MODE_SHIFT: u32 = 16; // [17:16]
const CONF0_CH0_POS_MODE_SHIFT: u32 = 18; // [19:18]
const CONF0_CH0_HCTRL_MODE_SHIFT: u32 = 20; // [21:20]
const CONF0_CH0_LCTRL_MODE_SHIFT: u32 = 22; // [23:22]

/// What a counter does on an edge of the signal input.
///
/// Values are the hardware encoding (`pcnt_channel_edge_action_t`): the field
/// documents `2'd1` as increase and `2'd2` as decrease, everything else a
/// no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeAction {
    /// Ignore this edge.
    Hold = 0,
    /// Step the counter up.
    Increase = 1,
    /// Step the counter down.
    Decrease = 2,
}

/// How the control input's *level* modifies the edge action.
///
/// Values are the hardware encoding (`pcnt_channel_level_action_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelAction {
    /// Use the edge action unchanged.
    Keep = 0,
    /// Swap increase and decrease.
    Inverse = 1,
    /// Freeze the counter while the control level holds.
    Hold = 2,
}

/// One channel's behaviour: what each edge does, and how the control level
/// bends it. A unit has two channels; a single-signal encoder uses channel 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelMode {
    /// Action on a rising edge of the signal.
    pub pos: EdgeAction,
    /// Action on a falling edge of the signal.
    pub neg: EdgeAction,
    /// Modifier applied while the control input is high.
    pub high: LevelAction,
    /// Modifier applied while the control input is low.
    pub low: LevelAction,
}

impl ChannelMode {
    /// The classic "signal edges step, control pin picks direction" encoder
    /// mode: count up on each rising edge while control is high, down while it
    /// is low.
    pub const UP_DOWN_ON_RISING: Self = Self {
        pos: EdgeAction::Increase,
        neg: EdgeAction::Hold,
        high: LevelAction::Keep,
        low: LevelAction::Inverse,
    };
}

/// The glitch filter setting: off, or on with a width in APB clock cycles.
///
/// A pulse narrower than `thres` cycles is discarded before it can reach the
/// counter. `thres` is clamped to [`MAX_FILTER_THRES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// Every edge reaches the counter.
    Off,
    /// Reject pulses shorter than this many APB cycles.
    Cycles(u16),
}

// ── Pure register encoding (host-testable) ──────────────────────────────────

/// Build the `conf0` value for channel 0 from a mode and filter setting.
///
/// Split out and `const` so the field placement — the part a wrong map corrupts
/// invisibly — is checked on a host, where there is no peripheral to read back.
/// The threshold/limit *event* enables (`thr_*_en`) stay zero: this driver runs
/// the counter as a free-wrapping 16-bit signed value, not against a watchpoint.
const fn conf0_bits(mode: ChannelMode, filter: Filter) -> u32 {
    let (filter_en, thres) = match filter {
        Filter::Off => (0, 0),
        Filter::Cycles(t) => {
            // Clamp without `min` — this is a const fn.
            let t = if t > MAX_FILTER_THRES { MAX_FILTER_THRES } else { t };
            (CONF0_FILTER_EN, t as u32)
        }
    };
    (thres & CONF0_FILTER_THRES_MASK)
        | filter_en
        | ((mode.neg as u32) << CONF0_CH0_NEG_MODE_SHIFT)
        | ((mode.pos as u32) << CONF0_CH0_POS_MODE_SHIFT)
        | ((mode.high as u32) << CONF0_CH0_HCTRL_MODE_SHIFT)
        | ((mode.low as u32) << CONF0_CH0_LCTRL_MODE_SHIFT)
}

/// Interpret the low 16 bits of a counter register as a signed value.
///
/// `cnt_val` is 16-bit two's complement in `cnt_unit[u]`; the top half is
/// reserved and must be masked off before the sign extension, or a stray upper
/// bit turns a small positive count into a large negative one.
const fn count_of(reg_val: u32) -> i16 {
    (reg_val & 0xFFFF) as u16 as i16
}

/// Matrix input signal index for unit `u`'s channel-0 *signal* input.
///
/// From esp-idf `soc/gpio_sig_map.h` (`PCNT_SIG_CH0_INn_IDX`). The indices are
/// contiguous for units 0..=4 (39, 43, 47, 51, 55) but jump for units 5..=7
/// (71, 75, 79) — hence a match rather than arithmetic, because the obvious
/// `39 + u*4` is wrong for exactly the last three units.
const fn sig_ch0_index(u: u8) -> Option<u32> {
    Some(match u {
        0..=4 => 39 + (u as u32) * 4,
        5..=7 => 71 + (u as u32 - 5) * 4,
        _ => return None,
    })
}

/// Matrix input signal index for unit `u`'s channel-0 *control* input
/// (`PCNT_CTRL_CH0_INn_IDX`): the signal index two above the corresponding
/// signal input.
const fn ctrl_ch0_index(u: u8) -> Option<u32> {
    match sig_ch0_index(u) {
        Some(sig) => Some(sig + 2),
        None => None,
    }
}

// ── The driver ──────────────────────────────────────────────────────────────

/// One configured PCNT unit.
pub struct PcntUnit {
    unit: u8,
}

impl PcntUnit {
    /// Bring unit `unit` up: clock it, hold it paused and zeroed, and program
    /// channel 0 with `mode` and `filter`. Route the inputs afterwards with
    /// [`route_signal`](Self::route_signal) / [`route_control`](Self::route_control),
    /// then [`resume`](Self::resume).
    ///
    /// # Safety
    /// Takes exclusive ownership of unit `unit`'s registers. The DPORT clock
    /// gate it toggles is shared, but [`dport::enable`] is cross-core safe.
    pub unsafe fn new(unit: u8, mode: ChannelMode, filter: Filter) -> BusResult<Self> {
        if unit >= NUM_UNITS {
            return Err(BusError::InvalidConfig);
        }
        // Without the clock and out of reset first, every write below lands
        // nowhere and every read comes back zero.
        dport::enable(dport::ClockBit::PCNT);

        let this = Self { unit };
        this.pause();
        this.clear();
        this.configure(mode, filter);
        Ok(this)
    }

    /// Rewrite channel 0's mode and filter. Cheap enough to call while running;
    /// used to flip direction or change filtering mid-stream.
    pub fn configure(&self, mode: ChannelMode, filter: Filter) {
        unsafe { reg::write(self.conf0(), conf0_bits(mode, filter)) };
    }

    /// Turn the glitch filter on (`Some`) or off (`None`) without disturbing the
    /// channel mode already in `conf0`.
    pub fn set_filter(&self, filter: Filter) {
        let thres = match filter {
            Filter::Off => 0,
            Filter::Cycles(t) => (t.min(MAX_FILTER_THRES)) as u32,
        };
        unsafe {
            // Clear the threshold and enable bits, then set the new ones,
            // leaving every mode field in the register alone.
            reg::modify(
                self.conf0(),
                CONF0_FILTER_THRES_MASK | CONF0_FILTER_EN,
                thres | if matches!(filter, Filter::Off) { 0 } else { CONF0_FILTER_EN },
            );
        }
    }

    /// Route this unit's channel-0 *signal* input to read from `pin`.
    ///
    /// Safe: `pin` is validated (a nonexistent pad gives
    /// [`BusError::InvalidConfig`]) and the matrix input is this unit's own.
    pub fn route_signal(&self, pin: u8) -> BusResult<()> {
        let idx = sig_ch0_index(self.unit).ok_or(BusError::InvalidConfig)?;
        // SAFETY: `pin` is validated inside `route_input` and `idx` is this
        // unit's own channel-0 signal index.
        unsafe { self.route_input(idx, pin) }
    }

    /// Route this unit's channel-0 *control* input to read from `pin`.
    ///
    /// Safe: as [`route_signal`](Self::route_signal).
    pub fn route_control(&self, pin: u8) -> BusResult<()> {
        let idx = ctrl_ch0_index(self.unit).ok_or(BusError::InvalidConfig)?;
        // SAFETY: as `route_signal`; `idx` is this unit's control index.
        unsafe { self.route_input(idx, pin) }
    }

    /// Tie this unit's channel-0 control input to a constant level, with no pad.
    ///
    /// The GPIO matrix can feed a peripheral input a fixed 0 or 1 without a real
    /// pin — which is how a fixed-direction counter selects its direction, and
    /// how the self-test picks up-then-down without a second wire.
    ///
    /// Safe: writes the matrix input register for this unit's own control
    /// signal, feeding it a constant source (no pad).
    pub fn route_control_level(&self, high: bool) -> BusResult<()> {
        let idx = ctrl_ch0_index(self.unit).ok_or(BusError::InvalidConfig)?;
        let src = if high {
            gpio_matrix::IN_CONST_ONE
        } else {
            gpio_matrix::IN_CONST_ZERO
        };
        // SAFETY: `idx` is this unit's control index; the matrix constant
        // sources are not GPIO numbers but fit the same six-bit field, which is
        // why `connect_input` accepts them.
        unsafe { gpio_matrix::connect_input(idx, src as u8, false) }
    }

    unsafe fn route_input(&self, idx: u32, pin: u8) -> BusResult<()> {
        // The pad has to be in GPIO function with its input buffer on for the
        // matrix to read it.
        io_mux::configure(pin, io_mux::gpio_function(pin), true, PinPull::None)?;
        gpio_matrix::connect_input(idx, pin, false)
    }

    /// Pause the counter without clearing it.
    pub fn pause(&self) {
        unsafe { reg::set(Self::ctrl_reg(), self.pause_bit()) };
    }

    /// Resume counting.
    pub fn resume(&self) {
        unsafe { reg::clear(Self::ctrl_reg(), self.pause_bit()) };
    }

    /// Reset the counter to zero. The reset is a pulse: assert then release, or
    /// the counter is held at zero rather than freed to count.
    pub fn clear(&self) {
        let bit = self.reset_bit();
        unsafe {
            reg::set(Self::ctrl_reg(), bit);
            reg::clear(Self::ctrl_reg(), bit);
        }
    }

    /// The current count, as a signed 16-bit value with direction (negative
    /// means the control level ran it backwards).
    pub fn count(&self) -> i16 {
        count_of(unsafe { reg::read(self.cnt()) })
    }

    // Register addresses for this unit.
    fn conf0(&self) -> *mut u32 {
        reg::at(PCNT_BASE, self.unit as u32 * UNIT_CONF_STRIDE)
    }
    fn cnt(&self) -> *mut u32 {
        reg::at(PCNT_BASE, CNT_BASE + self.unit as u32 * 4)
    }
    fn ctrl_reg() -> *mut u32 {
        reg::at(PCNT_BASE, CTRL)
    }
    /// `cnt_rst_uN` is at bit `2N` of `ctrl`.
    fn reset_bit(&self) -> u32 {
        1 << (2 * self.unit as u32)
    }
    /// `cnt_pause_uN` is at bit `2N + 1` of `ctrl`.
    fn pause_bit(&self) -> u32 {
        1 << (2 * self.unit as u32 + 1)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_down_mode_sets_pos_increase_and_low_inverse() {
        // The encoder mode this driver exists to serve. Encoded into conf0's
        // channel-0 fields: pos-mode = increase (1) at [19:18], low-ctrl =
        // inverse (1) at [23:22], neg-mode and high-ctrl left at 0.
        let v = conf0_bits(ChannelMode::UP_DOWN_ON_RISING, Filter::Off);
        assert_eq!((v >> CONF0_CH0_POS_MODE_SHIFT) & 0b11, 1); // Increase
        assert_eq!((v >> CONF0_CH0_NEG_MODE_SHIFT) & 0b11, 0); // Hold
        assert_eq!((v >> CONF0_CH0_HCTRL_MODE_SHIFT) & 0b11, 0); // Keep
        assert_eq!((v >> CONF0_CH0_LCTRL_MODE_SHIFT) & 0b11, 1); // Inverse
        // Filter off: no threshold, no enable.
        assert_eq!(v & CONF0_FILTER_THRES_MASK, 0);
        assert_eq!(v & CONF0_FILTER_EN, 0);
    }

    #[test]
    fn the_filter_lives_in_the_low_ten_bits_with_its_enable_above() {
        let v = conf0_bits(ChannelMode::UP_DOWN_ON_RISING, Filter::Cycles(100));
        assert_eq!(v & CONF0_FILTER_THRES_MASK, 100);
        assert_ne!(v & CONF0_FILTER_EN, 0);
        // The mode fields must survive the filter being set in the same word.
        assert_eq!((v >> CONF0_CH0_POS_MODE_SHIFT) & 0b11, 1);
    }

    #[test]
    fn the_filter_threshold_is_clamped_to_ten_bits() {
        // 2000 > 1023; a value that overran [9:0] would spill into filter_en
        // and the mode fields.
        let v = conf0_bits(ChannelMode::UP_DOWN_ON_RISING, Filter::Cycles(2000));
        assert_eq!(v & CONF0_FILTER_THRES_MASK, MAX_FILTER_THRES as u32);
        assert_eq!(v & !( CONF0_FILTER_THRES_MASK | CONF0_FILTER_EN
            | (0b11 << CONF0_CH0_POS_MODE_SHIFT)
            | (0b11 << CONF0_CH0_LCTRL_MODE_SHIFT)), 0);
    }

    #[test]
    fn decrease_and_hold_encode_as_two_and_zero() {
        let mode = ChannelMode {
            pos: EdgeAction::Decrease,
            neg: EdgeAction::Increase,
            high: LevelAction::Hold,
            low: LevelAction::Keep,
        };
        let v = conf0_bits(mode, Filter::Off);
        assert_eq!((v >> CONF0_CH0_POS_MODE_SHIFT) & 0b11, 2); // Decrease
        assert_eq!((v >> CONF0_CH0_NEG_MODE_SHIFT) & 0b11, 1); // Increase
        assert_eq!((v >> CONF0_CH0_HCTRL_MODE_SHIFT) & 0b11, 2); // Hold
        assert_eq!((v >> CONF0_CH0_LCTRL_MODE_SHIFT) & 0b11, 0); // Keep
    }

    #[test]
    fn a_signed_count_reads_back_negative_below_zero() {
        // The whole point of the peripheral: a count with direction. A counter
        // that ran backwards past zero reads as a negative number.
        assert_eq!(count_of(0x0000), 0);
        assert_eq!(count_of(0x0005), 5);
        assert_eq!(count_of(0xFFFF), -1);
        assert_eq!(count_of(0xFFFB), -5);
        assert_eq!(count_of(0x7FFF), 32767);
        assert_eq!(count_of(0x8000), -32768);
    }

    #[test]
    fn the_reserved_upper_half_of_the_counter_is_ignored() {
        // Only the low 16 bits are the count; a set reserved bit up top must
        // not flip the sign or the magnitude.
        assert_eq!(count_of(0xDEAD_0005), 5);
        assert_eq!(count_of(0x1234_FFFF), -1);
    }

    #[test]
    fn the_signal_indices_are_contiguous_then_jump() {
        // Units 0..=4 are the arithmetic run; 5..=7 are the exception the map
        // has and the naive formula misses.
        assert_eq!(sig_ch0_index(0), Some(39));
        assert_eq!(sig_ch0_index(4), Some(55));
        assert_eq!(sig_ch0_index(5), Some(71));
        assert_eq!(sig_ch0_index(7), Some(79));
        assert_eq!(sig_ch0_index(8), None);
        // Control is always the signal index + 2.
        assert_eq!(ctrl_ch0_index(0), Some(41));
        assert_eq!(ctrl_ch0_index(7), Some(81));
    }

    #[test]
    fn reset_and_pause_bits_are_a_pair_per_unit() {
        for u in 0..NUM_UNITS {
            let p = PcntUnit { unit: u };
            assert_eq!(p.reset_bit(), 1 << (2 * u as u32));
            assert_eq!(p.pause_bit(), 1 << (2 * u as u32 + 1));
            // The two must never collide — a reset that also paused, or a pause
            // that also reset, would make the counter untestable.
            assert_eq!(p.reset_bit() & p.pause_bit(), 0);
        }
    }

    #[test]
    fn unit_config_blocks_do_not_overlap_the_counters() {
        // Eight units × three 4-byte words = 0x60, which is exactly where the
        // counter block begins. A wrong stride would alias a unit's conf onto a
        // neighbour's counter.
        assert_eq!(NUM_UNITS as u32 * UNIT_CONF_STRIDE, CNT_BASE);
    }
}
