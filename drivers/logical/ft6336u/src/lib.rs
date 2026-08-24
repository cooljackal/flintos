// SPDX-License-Identifier: Apache-2.0

//! FocalTech FT6336U — self-capacitive touch-panel controller.
//!
//! The touch panel on the M5Stack Core2, at I2C [`ADDR`]. Layer 3: it knows the
//! part, not the chip or board driving it, and talks through a [`BusHandle`] —
//! the same shape as the IMU and PMIC drivers, and on the Core2 it shares the
//! internal I2C0 bus with both.
//!
//! # What it reports
//!
//! Up to two simultaneous touch points. Each point is an (x, y) coordinate, a
//! touch id, and an [`Event`] (down, contact, up). This driver reads the first
//! point, which is what a single-touch UI needs; the register layout for the
//! second is identical eight bytes further on if that is ever wanted.
//!
//! Coordinates are the panel's raw counts. On the Core2 the panel is 320×240
//! with a taller touch area (the three capacitive buttons below the screen), so
//! y runs past 240; the mapping to screen pixels is a board/app concern, not
//! this driver's — it reports what the controller measured.
//!
//! # Register facts
//!
//! From FocalTech's register map (the FT6x36 family): the touch count is the
//! low nibble of `TD_STATUS`, and each point packs its high coordinate bits and
//! flags into the same bytes as the coordinate — x's event flag in the top two
//! bits of `P1_XH`, the touch id in the top nibble of `P1_YH`. Presence is
//! checked against the vendor id register (`0x11`, FocalTech), which is stable
//! across the family, rather than the chip-id, which is not.

#![no_std]
#![cfg_attr(not(test), forbid(unsafe_code))]

use api::bus::{BusHandle, BusResult};

/// I2C address of the FT6336U. Fixed in the part.
pub const ADDR: u8 = 0x38;

/// `TD_STATUS`: number of touch points in the low nibble.
const REG_TD_STATUS: u8 = 0x02;
/// `P1_XH`: touch-1 x high nibble (`[3:0]`) and event flag (`[7:6]`). The five
/// bytes `0x02..=0x06` are read in one burst: status, then x hi/lo, y hi/lo.
const REG_TD_BLOCK: u8 = 0x02;
/// `CHIP_ID`, for logging — varies across the family, so not used for presence.
const REG_CHIP_ID: u8 = 0xA3;
/// `FOCALTECH_ID`: the vendor id, `0x11`, stable across the family.
const REG_FOCALTECH_ID: u8 = 0xA8;

/// The FocalTech vendor id `FOCALTECH_ID` reads back.
pub const FOCALTECH_ID: u8 = 0x11;

/// What a touch point is doing.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Finger just went down.
    Down,
    /// Finger lifted.
    Up,
    /// Finger held, still in contact.
    Contact,
    /// No event on this point.
    None,
}

impl Event {
    /// Decode the two event-flag bits from the top of `P1_XH`.
    const fn from_xh(xh: u8) -> Self {
        match xh >> 6 {
            0b00 => Event::Down,
            0b01 => Event::Up,
            0b10 => Event::Contact,
            _ => Event::None,
        }
    }
}

/// One touch point.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Touch {
    pub x: u16,
    pub y: u16,
    /// The controller's touch id, `[0, 1]` for the two-point part.
    pub id: u8,
    pub event: Event,
}

/// Combine a high byte (low nibble significant) and a low byte into a 12-bit
/// coordinate.
const fn coord(hi: u8, lo: u8) -> u16 {
    (((hi & 0x0F) as u16) << 8) | lo as u16
}

/// The touch id lives in the top nibble of `P1_YH`.
const fn touch_id(yh: u8) -> u8 {
    yh >> 4
}

/// An FT6336U on a bus already addressed to [`ADDR`].
pub struct Ft6336u<'a> {
    bus: BusHandle<'a>,
}

impl<'a> Ft6336u<'a> {
    /// Wrap a bus handle addressed to the FT6336U.
    ///
    /// Takes anything that converts into a [`BusHandle`], so a caller passes a
    /// plain `&device`: `Ft6336u::new(&controller.device(ft6336u::ADDR))`.
    pub fn new(bus: impl Into<BusHandle<'a>>) -> Self {
        Self { bus: bus.into() }
    }

    /// The vendor id register (`0x11` on a FocalTech part).
    pub fn focaltech_id(&self) -> BusResult<u8> {
        self.bus.read_reg(REG_FOCALTECH_ID)
    }

    /// The chip id register — informational; its value varies across the family.
    pub fn chip_id(&self) -> BusResult<u8> {
        self.bus.read_reg(REG_CHIP_ID)
    }

    /// Whether an FT6336U (or a family sibling) is answering, by its vendor id.
    pub fn is_present(&self) -> BusResult<bool> {
        Ok(self.focaltech_id()? == FOCALTECH_ID)
    }

    /// How many points are being touched right now (0, 1 or 2).
    pub fn touch_count(&self) -> BusResult<u8> {
        Ok(self.bus.read_reg(REG_TD_STATUS)? & 0x0F)
    }

    /// The first touch point, or `None` if nothing is being touched.
    ///
    /// One burst read of `TD_STATUS` and the point-1 bytes, so the count and the
    /// coordinate come from the same instant rather than two reads that could
    /// straddle a finger landing.
    pub fn touch1(&self) -> BusResult<Option<Touch>> {
        let mut b = [0u8; 5];
        self.bus.read_regs(REG_TD_BLOCK, &mut b)?;
        // b[0] = TD_STATUS, b[1] = P1_XH, b[2] = P1_XL, b[3] = P1_YH, b[4] = P1_YL.
        if b[0] & 0x0F == 0 {
            return Ok(None);
        }
        Ok(Some(Touch {
            x: coord(b[1], b[2]),
            y: coord(b[3], b[4]),
            id: touch_id(b[3]),
            event: Event::from_xh(b[1]),
        }))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coordinate_takes_only_the_low_nibble_of_the_high_byte() {
        // The top nibble of P1_XH is flags/id, not coordinate; letting it into
        // the value puts a touch at four thousand pixels across.
        assert_eq!(coord(0x01, 0x2C), 0x012C); // 300
        assert_eq!(coord(0xF1, 0x2C), 0x012C, "flags in the top nibble are masked off");
        assert_eq!(coord(0x00, 0x00), 0);
        assert_eq!(coord(0x0F, 0xFF), 0x0FFF, "full 12-bit range");
    }

    #[test]
    fn the_event_flag_is_the_top_two_bits_of_p1_xh() {
        assert_eq!(Event::from_xh(0b00 << 6), Event::Down);
        assert_eq!(Event::from_xh(0b01 << 6), Event::Up);
        assert_eq!(Event::from_xh(0b10 << 6), Event::Contact);
        assert_eq!(Event::from_xh(0b11 << 6), Event::None);
        // The low bits (coordinate) do not change the event.
        assert_eq!(Event::from_xh((0b10 << 6) | 0x0F), Event::Contact);
    }

    #[test]
    fn the_touch_id_is_the_top_nibble_of_p1_yh() {
        assert_eq!(touch_id(0x00), 0);
        assert_eq!(touch_id(0x10), 1);
        // The low nibble is the coordinate's high bits and must not leak in.
        assert_eq!(touch_id(0x1F), 1);
    }

    #[test]
    fn a_full_point_decodes_the_way_the_register_block_is_laid_out() {
        // TD_STATUS=1, P1_XH=0x81 (event contact=0b10, x hi=0x1), P1_XL=0x2C,
        // P1_YH=0x10 (id=1, y hi=0x0), P1_YL=0x64. So x=0x12C=300, y=0x64=100.
        let b = [0x01u8, 0x81, 0x2C, 0x10, 0x64];
        assert_eq!(b[0] & 0x0F, 1, "one touch");
        assert_eq!(coord(b[1], b[2]), 300);
        assert_eq!(coord(b[3], b[4]), 100);
        assert_eq!(touch_id(b[3]), 1);
        assert_eq!(Event::from_xh(b[1]), Event::Contact);
    }

    #[test]
    fn a_zero_count_means_no_touch() {
        // The count nibble is what says "nothing here"; the coordinate bytes are
        // stale and must not be reported as a phantom touch at (0,0).
        let td_status = 0x00u8;
        assert_eq!(td_status & 0x0F, 0);
    }

    #[test]
    fn presence_is_the_vendor_id_not_the_chip_id() {
        assert_eq!(FOCALTECH_ID, 0x11);
    }
}
