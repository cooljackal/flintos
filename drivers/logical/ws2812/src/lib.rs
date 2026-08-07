// SPDX-License-Identifier: Apache-2.0

//! WS2812 / SK6812 addressable LEDs.
//!
//! Layer 3: this knows the LED's wire protocol and nothing about the chip
//! producing the pulses. It turns colours into (level, duration) pairs and
//! hands them to whatever can emit them — on an ESP32 that is the RMT
//! peripheral, but nothing here says so.
//!
//! # The protocol
//!
//! One wire, no clock. Each bit is a fixed-period pulse whose *high* time
//! encodes the value:
//!
//! | Bit | High | Low | Tolerance |
//! |---|---|---|---|
//! | 0 | 350 ns | 800 ns | ±150 ns |
//! | 1 | 700 ns | 600 ns | ±150 ns |
//!
//! Then a reset: the line held low for at least 50 µs, which latches the
//! frame. Bits are sent most-significant first, and the byte order is **GRB**,
//! not RGB — the single most common way to get this wrong, and it fails by
//! showing the right brightness in the wrong colour, which reads like a
//! hardware fault.
//!
//! The tolerances are why this cannot be bit-banged from a task on a
//! preemptive kernel: a timer interrupt landing mid-bit stretches a pulse by
//! microseconds, and the LED reads whatever that turns into.

#![no_std]
#![cfg_attr(not(test), forbid(unsafe_code))]
//
// The layer check reads the dependency graph, and raw MMIO needs no
// dependency -- a device driver could write 0x3FF44008 with `api` as its only
// dep and still pass. This is the line that makes "cannot reach hardware" true
// rather than aspirational.
//
// Scoped to non-test builds because the mock buses these crates test against
// use `unsafe` to extend a stack borrow to 'static. That is test scaffolding
// and never ships; the shipping code in all three crates has no `unsafe`.

/// A colour, as the LED wants it.
///
/// Named by what it is rather than by the wire order, so callers do not have
/// to think about GRB. [`Rgb::to_grb`] does that once, here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const OFF: Self = Self { r: 0, g: 0, b: 0 };
    pub const RED: Self = Self { r: 255, g: 0, b: 0 };
    pub const GREEN: Self = Self { r: 0, g: 255, b: 0 };
    pub const BLUE: Self = Self { r: 0, g: 0, b: 255 };

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Wire order: green, red, blue.
    pub const fn to_grb(self) -> [u8; 3] {
        [self.g, self.r, self.b]
    }

    /// Scale all three channels by `pct` percent.
    ///
    /// These LEDs are uncomfortably bright at full scale, and an onboard one a
    /// few centimetres from someone's eyes especially so.
    pub const fn dim(self, pct: u8) -> Self {
        let p = if pct > 100 { 100 } else { pct } as u16;
        Self {
            r: ((self.r as u16 * p) / 100) as u8,
            g: ((self.g as u16 * p) / 100) as u8,
            b: ((self.b as u16 * p) / 100) as u8,
        }
    }
}

/// Nanosecond timings for one bit, either value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub zero_high_ns: u32,
    pub zero_low_ns: u32,
    pub one_high_ns: u32,
    pub one_low_ns: u32,
    /// Line held low to latch the frame.
    pub reset_us: u32,
}

impl Timing {
    /// The datasheet figures, which both WS2812B and SK6812 accept.
    pub const WS2812: Self = Self {
        zero_high_ns: 350,
        zero_low_ns: 800,
        one_high_ns: 700,
        one_low_ns: 600,
        reset_us: 80,
    };
}

/// One bit as a (high ticks, low ticks) pair, in units of `ns_per_tick`.
///
/// Separated from any peripheral so the conversion can be tested: a rounding
/// error here is a pulse outside tolerance, and the symptom is a wrong colour
/// rather than an error.
pub const fn bit_ticks(bit: bool, t: Timing, ns_per_tick: u32) -> (u16, u16) {
    let (h, l) = if bit {
        (t.one_high_ns, t.one_low_ns)
    } else {
        (t.zero_high_ns, t.zero_low_ns)
    };
    // Round to nearest: truncating a 350 ns pulse at 125 ns per tick gives 2
    // ticks (250 ns), which is outside the ±150 ns tolerance. Rounding gives 3
    // (375 ns), which is inside it.
    let ht = (h + ns_per_tick / 2) / ns_per_tick;
    let lt = (l + ns_per_tick / 2) / ns_per_tick;
    (ht as u16, lt as u16)
}

/// Number of bits a frame of `led_count` LEDs occupies.
pub const fn frame_bits(led_count: usize) -> usize {
    led_count * 24
}

/// Expand `colours` into per-bit (high, low) tick pairs, most-significant bit
/// first, in GRB order.
///
/// Returns how many entries were written, or `None` if `out` is too small —
/// never a partial frame. A truncated frame lights some LEDs and leaves the
/// rest holding their previous colour, which looks like a wiring fault.
pub fn encode(
    colours: &[Rgb],
    timing: Timing,
    ns_per_tick: u32,
    out: &mut [(u16, u16)],
) -> Option<usize> {
    let needed = frame_bits(colours.len());
    if out.len() < needed {
        return None;
    }
    let mut i = 0;
    for c in colours {
        for byte in c.to_grb() {
            for bit in (0..8).rev() {
                out[i] = bit_ticks(byte & (1 << bit) != 0, timing, ns_per_tick);
                i += 1;
            }
        }
    }
    Some(i)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const NS: u32 = 125; // the tick a divider of 10 gives on an 80 MHz APB

    #[test]
    fn the_wire_order_is_grb_not_rgb() {
        // The most common way to get these wrong, and it fails by showing the
        // right brightness in the wrong colour -- which reads as a hardware
        // fault rather than a byte-order bug.
        assert_eq!(Rgb::new(1, 2, 3).to_grb(), [2, 1, 3]);
        assert_eq!(Rgb::RED.to_grb(), [0, 255, 0], "red is the middle byte");
        assert_eq!(Rgb::GREEN.to_grb(), [255, 0, 0], "green is first");
    }

    #[test]
    fn bit_timings_round_to_nearest_and_stay_in_tolerance() {
        // Truncating 350 ns at 125 ns/tick gives 2 ticks = 250 ns, which is
        // 100 ns outside the +/-150 ns window. Rounding gives 3 = 375 ns.
        let (h, _) = bit_ticks(false, Timing::WS2812, NS);
        assert_eq!(h, 3);
        let ns = h as u32 * NS;
        assert!(ns.abs_diff(350) <= 150, "zero-bit high {ns} ns out of tolerance");

        let (h, _) = bit_ticks(true, Timing::WS2812, NS);
        let ns = h as u32 * NS;
        assert!(ns.abs_diff(700) <= 150, "one-bit high {ns} ns out of tolerance");
    }

    #[test]
    fn a_zero_and_a_one_are_distinguishable() {
        let (zh, _) = bit_ticks(false, Timing::WS2812, NS);
        let (oh, _) = bit_ticks(true, Timing::WS2812, NS);
        assert!(oh > zh, "a one must be held high longer than a zero");
    }

    #[test]
    fn each_encoded_period_matches_its_datasheet_period() {
        // Note the two periods are NOT equal: the datasheet specifies
        // 350+800 = 1150 ns for a zero and 700+600 = 1300 ns for a one. An
        // earlier version of this test assumed a constant period and failed
        // against correct code -- the LED tolerates the difference, and
        // encoding both to the same length would be the actual mistake.
        let t = Timing::WS2812;
        for (bit, want) in [
            (false, t.zero_high_ns + t.zero_low_ns),
            (true, t.one_high_ns + t.one_low_ns),
        ] {
            let (h, l) = bit_ticks(bit, t, NS);
            let got = (h as u32 + l as u32) * NS;
            assert!(
                got.abs_diff(want) <= 150,
                "bit {bit}: encoded period {got} ns vs datasheet {want} ns"
            );
        }
    }

    #[test]
    fn the_low_times_are_also_in_tolerance() {
        // The high time carries the value, but a low time out of spec runs the
        // bits together and the LED reads the frame as garbage.
        let t = Timing::WS2812;
        let (_, zl) = bit_ticks(false, t, NS);
        assert!((zl as u32 * NS).abs_diff(t.zero_low_ns) <= 150);
        let (_, ol) = bit_ticks(true, t, NS);
        assert!((ol as u32 * NS).abs_diff(t.one_low_ns) <= 150);
    }

    #[test]
    fn encoding_is_msb_first() {
        let mut out = [(0u16, 0u16); 24];
        // Green = 0x80: the first bit sent must be the one that is set.
        encode(&[Rgb::new(0, 0x80, 0)], Timing::WS2812, NS, &mut out).unwrap();
        let one = bit_ticks(true, Timing::WS2812, NS);
        let zero = bit_ticks(false, Timing::WS2812, NS);
        assert_eq!(out[0], one, "MSB first");
        assert_eq!(out[1], zero);
    }

    #[test]
    fn a_short_buffer_is_refused_rather_than_half_filled() {
        // A partial frame lights some LEDs and leaves the rest as they were,
        // which looks like a broken wire.
        let mut out = [(0u16, 0u16); 23];
        assert!(encode(&[Rgb::RED], Timing::WS2812, NS, &mut out).is_none());
    }

    #[test]
    fn a_frame_is_twenty_four_entries_per_led() {
        assert_eq!(frame_bits(1), 24);
        assert_eq!(frame_bits(8), 192);
        let mut out = [(0u16, 0u16); 48];
        assert_eq!(
            encode(&[Rgb::RED, Rgb::BLUE], Timing::WS2812, NS, &mut out),
            Some(48)
        );
    }

    #[test]
    fn dimming_scales_and_clamps() {
        assert_eq!(Rgb::new(100, 200, 50).dim(50), Rgb::new(50, 100, 25));
        assert_eq!(Rgb::RED.dim(0), Rgb::OFF);
        // Over 100% must not brighten past full scale or wrap.
        assert_eq!(Rgb::new(200, 0, 0).dim(200), Rgb::new(200, 0, 0));
    }
}
