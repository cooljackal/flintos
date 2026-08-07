// SPDX-License-Identifier: Apache-2.0

//! Chained LED panels: turning `(x, y)` into a position along the chain.
//!
//! A panel is a strip someone folded. The LEDs are wired in one line, and the
//! only thing separating a 5×5 grid from a 25-LED strip is knowing where the
//! line doubles back. Vendors fold them differently, so that knowledge is the
//! whole content of this crate.
//!
//! # Not a driver
//!
//! This lives in `lib/`, not `drivers/`. A driver knows a part number and its
//! output is destined for a pin: `ws2812` knows GRB order and 350 ns pulses,
//! `bme280` knows a humidity register. This knows no chip, no bus and no pin,
//! and its output is an integer. The same layouts describe APA102, SK9822 and
//! anything else wired as a chain.
//!
//! It depends on nothing — not even `api`. Calling it a driver would have been
//! a false statement, and the Layer-3 naming convention would have made the
//! claim out loud by handing it the name `driver-led-matrix`.
//!
//! # Coordinates
//!
//! `x` increases to the right, `y` increases **downward**, both from zero at
//! the top-left of the panel as you look at it. Screen convention, not
//! Cartesian, because it matches how anyone writing a font or a bitmap will
//! already be thinking.
//!
//! # Measure your panel, do not guess it
//!
//! There are 16 layouts and they all look plausible. Getting it wrong lights
//! the wrong pixel, which reads as a broken panel rather than a wrong constant.
//!
//! `apps/blink --features atom-matrix` walks the chain one LED at a time and
//! logs each index. Watch which cell lights, and you have measured it:
//!
//! - where index 0 sits → [`Origin`]
//! - whether index 1 is beside or below it → [`Axis`]
//! - whether each line restarts at the same edge or reverses → [`Order`]
//!
//! # Where a measured layout belongs
//!
//! Not here. A panel's geometry is a fact about a *board*, in the same way a
//! pin number is, and the board manifest is the single place those live so
//! that applications do not carry datasheet knowledge. `board::active`
//! declares the layout for a board that physically has a panel — see
//! `board/src/m5_atom_matrix.rs`.
//!
//! For a panel that is not part of any board, a strip you wired up yourself,
//! the application declares it. That is honest: nobody has measured it for
//! you.

#![no_std]

/// Which corner holds index 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Origin {
    /// Whether the chain runs right-to-left rather than left-to-right.
    const fn flips_x(self) -> bool {
        matches!(self, Origin::TopRight | Origin::BottomRight)
    }

    /// Whether lines stack upward rather than downward.
    const fn flips_y(self) -> bool {
        matches!(self, Origin::BottomLeft | Origin::BottomRight)
    }
}

/// Whether consecutive indices run along a row or down a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Index 1 is beside index 0.
    Rows,
    /// Index 1 is below index 0.
    Columns,
}

/// Whether alternate lines reverse direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Every line runs the same way; the chain jumps back to the same edge.
    /// Needs a return wire per line, so it is the more expensive panel to
    /// build and the less common one.
    Progressive,
    /// Alternate lines reverse, so the chain snakes. The default for most
    /// cheap panels because it needs no return wire.
    Zigzag,
}

/// How a panel is folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub width: usize,
    pub height: usize,
    pub origin: Origin,
    pub axis: Axis,
    pub order: Order,
}

impl Layout {
    pub const fn new(
        width: usize,
        height: usize,
        origin: Origin,
        axis: Axis,
        order: Order,
    ) -> Self {
        Self { width, height, origin, axis, order }
    }

    /// How many LEDs the panel has.
    pub const fn len(&self) -> usize {
        self.width * self.height
    }

    /// Whether the panel has no LEDs at all.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Position along the chain of the LED at `(x, y)`, or `None` if that cell
    /// is off the panel.
    ///
    /// Rejecting out-of-range rather than wrapping is deliberate: a wrapped
    /// coordinate lights a real LED somewhere else on the panel, which looks
    /// like a layout bug and hides the actual off-by-one in the caller.
    pub const fn index(&self, x: usize, y: usize) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }

        // Fold the origin into the top-left corner first, so the line/position
        // arithmetic below only ever has one case.
        let fx = if self.origin.flips_x() { self.width - 1 - x } else { x };
        let fy = if self.origin.flips_y() { self.height - 1 - y } else { y };

        let (line, pos, line_len) = match self.axis {
            Axis::Rows => (fy, fx, self.width),
            Axis::Columns => (fx, fy, self.height),
        };

        // A zigzag reverses every other line. Progressive panels jump back.
        let pos = match self.order {
            Order::Zigzag if line % 2 == 1 => line_len - 1 - pos,
            _ => pos,
        };

        Some(line * line_len + pos)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Bottom-right origin, row-major, progressive.
    ///
    /// Named here as a shape rather than as a board: the claim that a
    /// particular panel *is* this shape is a fact about that board, and lives
    /// with the board. What belongs here is that the shape maps the way the
    /// name says.
    const BR_ROWS_PROGRESSIVE: Layout =
        Layout::new(5, 5, Origin::BottomRight, Axis::Rows, Order::Progressive);

    const ORIGINS: [Origin; 4] = [
        Origin::TopLeft,
        Origin::TopRight,
        Origin::BottomLeft,
        Origin::BottomRight,
    ];
    const AXES: [Axis; 2] = [Axis::Rows, Axis::Columns];
    const ORDERS: [Order; 2] = [Order::Progressive, Order::Zigzag];

    /// Every layout, at a couple of panel shapes including a non-square one.
    fn all_layouts() -> impl Iterator<Item = Layout> {
        [(5usize, 5usize), (8, 8), (4, 7)].into_iter().flat_map(|(w, h)| {
            ORIGINS.iter().flat_map(move |o| {
                AXES.iter().flat_map(move |a| {
                    ORDERS.iter().map(move |r| Layout::new(w, h, *o, *a, *r))
                })
            })
        })
    }

    #[test]
    fn a_bottom_right_progressive_panel_runs_leftward_then_upward() {
        let m = BR_ROWS_PROGRESSIVE;
        assert_eq!(m.index(4, 4), Some(0), "index 0 is bottom-right");
        assert_eq!(m.index(3, 4), Some(1), "runs leftward along the bottom");
        assert_eq!(m.index(0, 4), Some(4), "bottom-left ends the first row");
        assert_eq!(m.index(4, 3), Some(5), "next row up restarts at the right");
        assert_eq!(m.index(0, 0), Some(24), "top-left is the far end");
    }

    #[test]
    fn a_bottom_right_progressive_panel_is_a_top_left_one_rotated_half_a_turn() {
        // A second, independent expression of the same mapping. If the
        // rotation identity and the corner checks ever disagree, one of them
        // is wrong, and the test says which pair to look at.
        let m = BR_ROWS_PROGRESSIVE;
        for y in 0..5 {
            for x in 0..5 {
                assert_eq!(m.index(x, y), Some(24 - (y * 5 + x)), "at ({x},{y})");
            }
        }
    }

    #[test]
    fn progressive_and_zigzag_are_distinguishable_from_the_second_line_on() {
        // The two are identical on line 0, which is why guessing between them
        // from a single lit LED does not work.
        let zig = Layout::new(5, 5, Origin::BottomRight, Axis::Rows, Order::Zigzag);
        assert_eq!(zig.index(4, 4), BR_ROWS_PROGRESSIVE.index(4, 4), "line 0 agrees");
        assert_eq!(zig.index(0, 3), Some(5), "zigzag comes back the other way");
        assert_ne!(zig.index(4, 3), BR_ROWS_PROGRESSIVE.index(4, 3));
    }

    #[test]
    fn every_layout_maps_each_cell_to_a_distinct_index() {
        // The property that makes a layout usable at all. An off-by-one in a
        // fold sends two cells to one index, which lights the wrong pixel and
        // leaves another dark -- and reads as a dead LED, not a mapping bug.
        for layout in all_layouts() {
            let mut seen = [false; 64];
            for y in 0..layout.height {
                for x in 0..layout.width {
                    let i = layout.index(x, y).expect("in range");
                    assert!(i < layout.len(), "{layout:?} at ({x},{y}) gave {i}, off the chain");
                    assert!(!seen[i], "{layout:?}: two cells map to {i}");
                    seen[i] = true;
                }
            }
            // Surjective as well as injective: every LED reachable.
            for (i, hit) in seen.iter().take(layout.len()).enumerate() {
                assert!(hit, "{layout:?}: nothing maps to index {i}");
            }
        }
    }

    #[test]
    fn a_cell_off_the_panel_is_refused_not_wrapped() {
        // Wrapping would light a real LED elsewhere, which hides the caller's
        // off-by-one behind something that looks like a layout bug.
        let m = BR_ROWS_PROGRESSIVE;
        assert_eq!(m.index(5, 0), None);
        assert_eq!(m.index(0, 5), None);
        assert_eq!(m.index(usize::MAX, 0), None);
    }

    #[test]
    fn zigzag_reverses_only_the_odd_lines() {
        let z = Layout::new(4, 3, Origin::TopLeft, Axis::Rows, Order::Zigzag);
        // Row 0 runs left to right.
        assert_eq!(z.index(0, 0), Some(0));
        assert_eq!(z.index(3, 0), Some(3));
        // Row 1 comes back the other way.
        assert_eq!(z.index(3, 1), Some(4));
        assert_eq!(z.index(0, 1), Some(7));
        // Row 2 is left to right again.
        assert_eq!(z.index(0, 2), Some(8));
    }

    #[test]
    fn columns_walk_down_before_across() {
        let c = Layout::new(4, 3, Origin::TopLeft, Axis::Columns, Order::Progressive);
        assert_eq!(c.index(0, 0), Some(0));
        assert_eq!(c.index(0, 1), Some(1), "index 1 is below index 0");
        assert_eq!(c.index(0, 2), Some(2));
        assert_eq!(c.index(1, 0), Some(3), "next column starts at the top");
    }

    #[test]
    fn a_non_square_panel_uses_the_right_line_length() {
        // Rows step by width, columns by height. Getting these the wrong way
        // round happens to work on a square panel and fails everywhere else,
        // which is why the layout sweep above includes a 4x7.
        let rows = Layout::new(4, 7, Origin::TopLeft, Axis::Rows, Order::Progressive);
        assert_eq!(rows.index(0, 1), Some(4), "a row is `width` long");
        let cols = Layout::new(4, 7, Origin::TopLeft, Axis::Columns, Order::Progressive);
        assert_eq!(cols.index(1, 0), Some(7), "a column is `height` long");
    }

    #[test]
    fn each_origin_puts_index_zero_in_its_own_corner() {
        for origin in ORIGINS {
            let l = Layout::new(5, 5, origin, Axis::Rows, Order::Progressive);
            let (x, y) = match origin {
                Origin::TopLeft => (0, 0),
                Origin::TopRight => (4, 0),
                Origin::BottomLeft => (0, 4),
                Origin::BottomRight => (4, 4),
            };
            assert_eq!(l.index(x, y), Some(0), "{origin:?}");
        }
    }
}
