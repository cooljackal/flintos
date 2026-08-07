// SPDX-License-Identifier: Apache-2.0

//! What an addressable LED strip promises, and code written against it.
//!
//! A `lib/` crate: no registers, no part numbers, no pins. It defines the
//! contract that LED drivers keep, and the effects worth writing once rather
//! than once per chip.
//!
//! # The promises are small on purpose
//!
//! One fat trait forces all-or-nothing. A chip either implements everything —
//! faking whatever it lacks — or it implements nothing and looks unsupported.
//! Both are lies.
//!
//! Split up, a driver states exactly what its hardware does:
//!
//! | Trait | Meaning | WS2812 | APA102 |
//! |---|---|---|---|
//! | [`LedStrip`] | set a pixel, push the frame | yes | yes |
//! | [`Dimmable`] | a global brightness the *hardware* applies | no | yes, 5 bits |
//!
//! WS2812 has no brightness register — dimming it means scaling the colour
//! before sending, which costs colour depth. APA102 has one. If both
//! implemented a single `LedStrip` with `set_brightness` on it, one of them
//! would have to pretend, and a caller could not tell which.
//!
//! Not implementing a trait is therefore a real statement, and
//! `make device-matrix` prints who implements what so a gap is visible rather
//! than merely absent.

#![no_std]
#![cfg_attr(not(test), forbid(unsafe_code))]

/// A colour.
///
/// Lives here rather than in a driver because a colour is not a property of
/// any part number — it was in `ws2812`, which meant using an APA102 would
/// have meant depending on a WS2812 driver to name a colour.
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
    pub const WHITE: Self = Self { r: 255, g: 255, b: 255 };

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Scale all three channels by `pct` percent.
    ///
    /// Software dimming: it costs colour depth, which is why [`Dimmable`]
    /// exists for chips that can do it in hardware instead.
    pub const fn dim(self, pct: u8) -> Self {
        let p = if pct > 100 { 100 } else { pct } as u16;
        Self {
            r: ((self.r as u16 * p) / 100) as u8,
            g: ((self.g as u16 * p) / 100) as u8,
            b: ((self.b as u16 * p) / 100) as u8,
        }
    }

    /// A colour from a position on the hue wheel, fully saturated.
    ///
    /// The one piece of colour maths every LED project rewrites. Integer only:
    /// there is no FPU on this class of part, and a software float here would
    /// cost more than the effect.
    pub const fn from_hue(hue: u8) -> Self {
        // Three 85-wide sectors, each fading one channel out and another in.
        let sector = hue / 85;
        let pos = (hue % 85) as u16;
        // Scale 0..84 to 0..255 without dividing: *3 is close enough that the
        // error is under two levels, and invisible on an LED.
        let up = (pos * 3) as u8;
        let down = 255 - up;
        match sector {
            0 => Self::new(down, up, 0),
            1 => Self::new(0, down, up),
            _ => Self::new(up, 0, down),
        }
    }
}

/// Something went wrong talking to the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripError {
    /// Pixel index past the end of the strip.
    OutOfRange,
    /// The underlying transport refused or failed.
    Transport,
}

/// The minimum an addressable strip must do.
///
/// Deliberately does not include brightness, colour order, or timing — those
/// are either the chip's business or a separate promise.
pub trait LedStrip {
    /// How many pixels the strip has.
    fn len(&self) -> usize;

    /// Whether the strip has no pixels.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Stage one pixel. Takes effect on [`LedStrip::show`].
    ///
    /// Staging rather than writing immediately, because these strips are
    /// clocked as a whole frame: there is no way to update one pixel on the
    /// wire, and pretending otherwise would send a full frame per call.
    fn set(&mut self, index: usize, colour: Rgb) -> Result<(), StripError>;

    /// Push the staged frame to the hardware.
    fn show(&mut self) -> Result<(), StripError>;

    /// Stage every pixel to one colour.
    fn fill(&mut self, colour: Rgb) -> Result<(), StripError> {
        for i in 0..self.len() {
            self.set(i, colour)?;
        }
        Ok(())
    }

    /// Stage every pixel off. Does not [`LedStrip::show`].
    fn clear(&mut self) -> Result<(), StripError> {
        self.fill(Rgb::OFF)
    }
}

/// A strip whose *hardware* applies a global brightness.
///
/// Separate from [`LedStrip`] because WS2812 has no such register. A chip that
/// does not implement this is saying so, rather than silently scaling colours
/// and losing depth — the caller can then choose [`Rgb::dim`] knowingly.
pub trait Dimmable {
    /// Set global brightness, 0 = off, 255 = full.
    ///
    /// Implementations quantise to whatever the hardware has; APA102 has 5
    /// bits, so 32 distinct levels.
    fn set_brightness(&mut self, level: u8) -> Result<(), StripError>;
}

/// Paint a hue gradient across the whole strip. Does not `show`.
///
/// Written once here rather than once per chip, which is the entire reason
/// [`LedStrip`] exists.
pub fn gradient<S: LedStrip + ?Sized>(strip: &mut S, start_hue: u8) -> Result<(), StripError> {
    let n = strip.len();
    if n == 0 {
        return Ok(());
    }
    for i in 0..n {
        // Spread one full turn of the wheel over the strip.
        let hue = start_hue.wrapping_add(((i * 256) / n) as u8);
        strip.set(i, Rgb::from_hue(hue))?;
    }
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A strip that exists only in memory, so the trait's own behaviour can be
    /// tested without a chip.
    struct FakeStrip {
        pixels: [Rgb; 8],
        shown: usize,
    }

    impl FakeStrip {
        fn new() -> Self {
            Self { pixels: [Rgb::OFF; 8], shown: 0 }
        }
    }

    impl LedStrip for FakeStrip {
        fn len(&self) -> usize {
            self.pixels.len()
        }
        fn set(&mut self, index: usize, colour: Rgb) -> Result<(), StripError> {
            *self.pixels.get_mut(index).ok_or(StripError::OutOfRange)? = colour;
            Ok(())
        }
        fn show(&mut self) -> Result<(), StripError> {
            self.shown += 1;
            Ok(())
        }
    }

    #[test]
    fn a_pixel_past_the_end_is_refused_not_wrapped() {
        // Wrapping would light a real pixel elsewhere, which reads as a broken
        // strip rather than an off-by-one in the caller.
        let mut s = FakeStrip::new();
        assert_eq!(s.set(8, Rgb::RED), Err(StripError::OutOfRange));
        assert_eq!(s.set(7, Rgb::RED), Ok(()));
    }

    #[test]
    fn staging_does_not_push_a_frame() {
        // One frame per `set` would clock the whole strip 25 times to draw 25
        // pixels. The default helpers must not do it either.
        let mut s = FakeStrip::new();
        s.fill(Rgb::BLUE).unwrap();
        gradient(&mut s, 0).unwrap();
        assert_eq!(s.shown, 0, "nothing should have been pushed yet");
        s.show().unwrap();
        assert_eq!(s.shown, 1);
    }

    #[test]
    fn fill_and_clear_cover_every_pixel() {
        let mut s = FakeStrip::new();
        s.fill(Rgb::GREEN).unwrap();
        assert!(s.pixels.iter().all(|p| *p == Rgb::GREEN));
        s.clear().unwrap();
        assert!(s.pixels.iter().all(|p| *p == Rgb::OFF));
    }

    #[test]
    fn a_gradient_covers_the_strip_without_repeating() {
        let mut s = FakeStrip::new();
        gradient(&mut s, 0).unwrap();
        // Adjacent pixels must differ, or the "gradient" is a fill.
        for w in s.pixels.windows(2) {
            assert_ne!(w[0], w[1], "gradient produced two identical neighbours");
        }
    }

    #[test]
    fn a_gradient_on_an_empty_strip_is_not_a_divide_by_zero() {
        struct Empty;
        impl LedStrip for Empty {
            fn len(&self) -> usize {
                0
            }
            fn set(&mut self, _: usize, _: Rgb) -> Result<(), StripError> {
                Err(StripError::OutOfRange)
            }
            fn show(&mut self) -> Result<(), StripError> {
                Ok(())
            }
        }
        // `(i * 256) / n` divides by the length.
        assert_eq!(gradient(&mut Empty, 0), Ok(()));
        assert!(Empty.is_empty());
    }

    #[test]
    fn the_hue_wheel_returns_to_where_it_started() {
        // A wheel with a seam shows as a visible jump in any rotating effect.
        let start = Rgb::from_hue(0);
        let end = Rgb::from_hue(254);
        let gap = (start.r as i16 - end.r as i16).abs()
            + (start.g as i16 - end.g as i16).abs()
            + (start.b as i16 - end.b as i16).abs();
        assert!(gap < 24, "hue wheel has a seam: {start:?} vs {end:?}");
    }

    #[test]
    fn each_third_of_the_wheel_is_a_primary() {
        assert_eq!(Rgb::from_hue(0), Rgb::RED);
        // 85 and 170 land on the sector boundaries.
        assert_eq!(Rgb::from_hue(85), Rgb::GREEN);
        assert_eq!(Rgb::from_hue(170), Rgb::BLUE);
    }

    #[test]
    fn dimming_scales_and_clamps() {
        assert_eq!(Rgb::new(100, 200, 50).dim(50), Rgb::new(50, 100, 25));
        assert_eq!(Rgb::RED.dim(0), Rgb::OFF);
        // Over 100% must not brighten past full scale or wrap.
        assert_eq!(Rgb::new(200, 0, 0).dim(200), Rgb::new(200, 0, 0));
    }
}
