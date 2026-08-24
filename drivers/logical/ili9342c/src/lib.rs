// SPDX-License-Identifier: Apache-2.0

//! ILI9342C — a 320×240 RGB565 SPI TFT controller.
//!
//! The display on the M5Stack Core2. Layer 3: it knows the ILI9342C command set
//! and pixel format, not the SPI controller or DMA engine underneath — those
//! live behind a [`DisplayInterface`], which the board builds from its SPI +
//! GPIO + DMA and hands in. So this driver is portable to any board that wires
//! an ILI9342C, and the fast path (DMA, chip-select batching) is the interface's
//! job, not repeated here.
//!
//! # Performance model
//!
//! There is no framebuffer. A 320×240×2 frame is 150 KB — more than the ESP32's
//! DMA-reachable DRAM — so this driver *streams*: it opens one SPI transaction
//! per fill or blit ([`DisplayInterface::start`]/[`end`]), sets the address
//! window, and lets the interface DMA the pixels. A solid fill costs no
//! per-pixel CPU work (the interface repeats one packed color); a blit hands the
//! interface the caller's pixels and the interface overlaps converting the next
//! chunk with transferring the current one. This is the same "no framebuffer,
//! stream windowed regions" model the fast Arduino libraries (TFT_eSPI,
//! LovyanGFX) use, and it is what a future higher-level (LVGL-style) layer would
//! call [`blit`](Ili9342c::blit) through.
//!
//! # Configuring orientation
//!
//! [`Config`] carries rotation, RGB/BGR order and inversion — the three things
//! boards disagree about. The Core2 runs BGR with inversion on; [`Config::CORE2`]
//! is that, and [`Config::default`] matches it.

#![no_std]
#![cfg_attr(not(test), forbid(unsafe_code))]

use api::display::{DisplayError, DisplayInterface};

// ── Colors ───────────────────────────────────────────────────────────────────

/// RGB565 color helpers. Pixels are plain `u16` on this driver's surface (so a
/// blit buffer is a `&[u16]` with no conversion), and this module builds them.
pub mod color {
    /// Pack 8-bit r/g/b into RGB565.
    pub const fn rgb565(r: u8, g: u8, b: u8) -> u16 {
        (((r as u16) & 0xF8) << 8) | (((g as u16) & 0xFC) << 3) | ((b as u16) >> 3)
    }

    pub const BLACK: u16 = 0x0000;
    pub const WHITE: u16 = 0xFFFF;
    pub const RED: u16 = rgb565(255, 0, 0);
    pub const GREEN: u16 = rgb565(0, 255, 0);
    pub const BLUE: u16 = rgb565(0, 0, 255);
    pub const YELLOW: u16 = rgb565(255, 255, 0);
    pub const CYAN: u16 = rgb565(0, 255, 255);
    pub const MAGENTA: u16 = rgb565(255, 0, 255);
    pub const ORANGE: u16 = rgb565(255, 165, 0);
}

// ── Command set ──────────────────────────────────────────────────────────────

const CMD_SWRESET: u8 = 0x01;
const CMD_SLPOUT: u8 = 0x11;
const CMD_INVOFF: u8 = 0x20;
const CMD_INVON: u8 = 0x21;
const CMD_DISPON: u8 = 0x29;
const CMD_CASET: u8 = 0x2A;
const CMD_PASET: u8 = 0x2B;
const CMD_RAMWR: u8 = 0x2C;
const CMD_MADCTL: u8 = 0x36;
const CMD_COLMOD: u8 = 0x3A;
const CMD_NORON: u8 = 0x13;

/// `COLMOD` value for 16-bit / 65K color (RGB565).
const COLMOD_16BIT: u8 = 0x55;

// MADCTL bits.
const MADCTL_MY: u8 = 0x80;
const MADCTL_MX: u8 = 0x40;
const MADCTL_MV: u8 = 0x20;
const MADCTL_BGR: u8 = 0x08;

// ── Configuration ────────────────────────────────────────────────────────────

/// Display rotation, in 90° steps clockwise from the panel's native landscape.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Rotation {
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

/// Whether the panel's subpixels are wired red-first or blue-first.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ColorOrder {
    Rgb,
    Bgr,
}

/// Board-specific display facts: native size, rotation, color order, inversion.
#[derive(Copy, Clone, Debug)]
pub struct Config {
    /// Native panel width (before rotation). The ILI9342C is a landscape part:
    /// 320.
    pub width: u16,
    /// Native panel height: 240.
    pub height: u16,
    pub rotation: Rotation,
    pub color_order: ColorOrder,
    /// Whether to enable display inversion. The Core2 panel needs it on.
    pub invert: bool,
}

impl Config {
    /// The M5Stack Core2's panel: 320×240, BGR, inversion on, native landscape.
    pub const CORE2: Config = Config {
        width: 320,
        height: 240,
        rotation: Rotation::Deg0,
        color_order: ColorOrder::Bgr,
        invert: true,
    };
}

impl Default for Config {
    fn default() -> Self {
        Config::CORE2
    }
}

/// The `MADCTL` byte for a rotation and color order.
const fn madctl(rotation: Rotation, order: ColorOrder) -> u8 {
    // The panel is native landscape, so Deg0 sets no axis bits. 90/270 set MV
    // (swap axes); 180/270 flip via MX/MY. Verified against the panel — see the
    // orientation notes in the module docs.
    let bits = match rotation {
        Rotation::Deg0 => 0,
        Rotation::Deg90 => MADCTL_MV | MADCTL_MX,
        Rotation::Deg180 => MADCTL_MX | MADCTL_MY,
        Rotation::Deg270 => MADCTL_MV | MADCTL_MY,
    };
    let order_bit = match order {
        ColorOrder::Rgb => 0,
        ColorOrder::Bgr => MADCTL_BGR,
    };
    bits | order_bit
}

/// Whether a rotation swaps width and height.
const fn swaps_axes(rotation: Rotation) -> bool {
    matches!(rotation, Rotation::Deg90 | Rotation::Deg270)
}

// ── Driver ───────────────────────────────────────────────────────────────────

/// An ILI9342C on a [`DisplayInterface`].
pub struct Ili9342c<I: DisplayInterface> {
    iface: I,
    cfg: Config,
    /// Effective dimensions after rotation.
    width: u16,
    height: u16,
}

impl<I: DisplayInterface> Ili9342c<I> {
    /// Wrap an interface. Call [`init`](Self::init) before drawing.
    pub fn new(iface: I, cfg: Config) -> Self {
        let (width, height) = if swaps_axes(cfg.rotation) {
            (cfg.height, cfg.width)
        } else {
            (cfg.width, cfg.height)
        };
        Self { iface, cfg, width, height }
    }

    /// The visible width after rotation.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// The visible height after rotation.
    pub fn height(&self) -> u16 {
        self.height
    }

    /// Reset and initialise the panel.
    ///
    /// `reset` drives the hardware reset line — `reset(true)` asserts it (low),
    /// `reset(false)` releases it. On the Core2 that line is an AXP192 GPIO, so
    /// the caller passes a closure that toggles it through the PMIC; a board that
    /// ties reset high passes a no-op. `delay_ms` is the RTOS's millisecond sleep
    /// — the datasheet's reset and sleep-out settling times belong to the caller,
    /// not this driver.
    pub fn init(
        &mut self,
        mut reset: impl FnMut(bool),
        mut delay_ms: impl FnMut(u32),
    ) -> Result<(), DisplayError> {
        // Hardware reset pulse, then a software reset for good measure.
        reset(true);
        delay_ms(10);
        reset(false);
        delay_ms(120);

        self.write_command(CMD_SWRESET, &[])?;
        delay_ms(150);
        self.write_command(CMD_SLPOUT, &[])?;
        delay_ms(120);
        self.write_command(CMD_COLMOD, &[COLMOD_16BIT])?;
        self.write_command(CMD_MADCTL, &[madctl(self.cfg.rotation, self.cfg.color_order)])?;
        self.write_command(if self.cfg.invert { CMD_INVON } else { CMD_INVOFF }, &[])?;
        self.write_command(CMD_NORON, &[])?;
        delay_ms(10);
        self.write_command(CMD_DISPON, &[])?;
        delay_ms(10);
        Ok(())
    }

    /// Fill the whole screen with one color.
    pub fn fill_screen(&mut self, color: u16) -> Result<(), DisplayError> {
        self.fill_rect(0, 0, self.width, self.height, color)
    }

    /// Fill a rectangle with one color. A no-op if the rectangle is empty.
    pub fn fill_rect(
        &mut self,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        color: u16,
    ) -> Result<(), DisplayError> {
        if w == 0 || h == 0 {
            return Ok(());
        }
        self.check_bounds(x, y, w, h)?;
        self.iface.start()?;
        self.set_window(x, y, x + w - 1, y + h - 1)?;
        self.iface.fill(color, w as usize * h as usize)?;
        self.iface.end()
    }

    /// Copy a buffer of RGB565 pixels into a rectangle, row-major from the
    /// top-left. `pixels.len()` must equal `w * h`.
    pub fn blit(
        &mut self,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        pixels: &[u16],
    ) -> Result<(), DisplayError> {
        if w as usize * h as usize != pixels.len() {
            return Err(DisplayError::SizeMismatch);
        }
        if w == 0 || h == 0 {
            return Ok(());
        }
        self.check_bounds(x, y, w, h)?;
        self.iface.start()?;
        self.set_window(x, y, x + w - 1, y + h - 1)?;
        self.iface.pixels(pixels)?;
        self.iface.end()
    }

    /// Set a single pixel. Convenience over [`fill_rect`](Self::fill_rect) of a
    /// 1×1 region — not a fast path; a run of these is far slower than one blit.
    pub fn draw_pixel(&mut self, x: u16, y: u16, color: u16) -> Result<(), DisplayError> {
        self.fill_rect(x, y, 1, 1, color)
    }

    // ── internals ────────────────────────────────────────────────────────────

    /// One self-contained command transaction (its own held CS). For init and
    /// register writes; fills/blits open their own transaction so the pixel
    /// stream shares the window's CS.
    fn write_command(&mut self, cmd: u8, args: &[u8]) -> Result<(), DisplayError> {
        self.iface.start()?;
        self.iface.command(cmd)?;
        if !args.is_empty() {
            self.iface.data(args)?;
        }
        self.iface.end()
    }

    /// Set the address window and leave the controller expecting pixel data.
    /// Assumes a transaction is already open (the pixel write must share its CS).
    fn set_window(&mut self, x0: u16, y0: u16, x1: u16, y1: u16) -> Result<(), DisplayError> {
        self.iface.command(CMD_CASET)?;
        self.iface
            .data(&[(x0 >> 8) as u8, x0 as u8, (x1 >> 8) as u8, x1 as u8])?;
        self.iface.command(CMD_PASET)?;
        self.iface
            .data(&[(y0 >> 8) as u8, y0 as u8, (y1 >> 8) as u8, y1 as u8])?;
        self.iface.command(CMD_RAMWR)
    }

    /// Reject a rectangle that runs off the visible area, before any bytes move.
    fn check_bounds(&self, x: u16, y: u16, w: u16, h: u16) -> Result<(), DisplayError> {
        if x as u32 + w as u32 > self.width as u32 || y as u32 + h as u32 > self.height as u32 {
            return Err(DisplayError::OutOfBounds);
        }
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    /// A recording interface: every call is logged, so a test can assert exactly
    /// what bytes a draw would put on the wire.
    #[derive(Default)]
    struct Rec {
        log: Vec<Op>,
    }
    #[derive(Debug, PartialEq, Eq)]
    enum Op {
        Start,
        End,
        Cmd(u8),
        Data(Vec<u8>),
        Fill(u16, usize),
        Pixels(usize),
    }
    impl DisplayInterface for Rec {
        fn start(&mut self) -> Result<(), DisplayError> {
            self.log.push(Op::Start);
            Ok(())
        }
        fn end(&mut self) -> Result<(), DisplayError> {
            self.log.push(Op::End);
            Ok(())
        }
        fn command(&mut self, cmd: u8) -> Result<(), DisplayError> {
            self.log.push(Op::Cmd(cmd));
            Ok(())
        }
        fn data(&mut self, bytes: &[u8]) -> Result<(), DisplayError> {
            self.log.push(Op::Data(bytes.to_vec()));
            Ok(())
        }
        fn fill(&mut self, color: u16, count: usize) -> Result<(), DisplayError> {
            self.log.push(Op::Fill(color, count));
            Ok(())
        }
        fn pixels(&mut self, pixels: &[u16]) -> Result<(), DisplayError> {
            self.log.push(Op::Pixels(pixels.len()));
            Ok(())
        }
    }

    fn core2() -> Ili9342c<Rec> {
        Ili9342c::new(Rec::default(), Config::CORE2)
    }

    #[test]
    fn rgb565_packs_the_channels_where_the_panel_expects() {
        assert_eq!(color::rgb565(0, 0, 0), 0x0000);
        assert_eq!(color::rgb565(255, 255, 255), 0xFFFF);
        assert_eq!(color::RED, 0xF800);
        assert_eq!(color::GREEN, 0x07E0);
        assert_eq!(color::BLUE, 0x001F);
    }

    #[test]
    fn native_size_is_landscape_and_rotation_swaps_it() {
        assert_eq!((core2().width(), core2().height()), (320, 240));
        let portrait = Ili9342c::new(
            Rec::default(),
            Config { rotation: Rotation::Deg90, ..Config::CORE2 },
        );
        assert_eq!((portrait.width(), portrait.height()), (240, 320));
    }

    #[test]
    fn the_core2_madctl_is_bgr_landscape() {
        // Deg0 sets no axis bits; BGR sets 0x08. Getting this wrong shows as a
        // mirrored image or swapped red/blue.
        assert_eq!(madctl(Rotation::Deg0, ColorOrder::Bgr), 0x08);
        assert_eq!(madctl(Rotation::Deg0, ColorOrder::Rgb), 0x00);
        assert_eq!(madctl(Rotation::Deg90, ColorOrder::Bgr), MADCTL_MV | MADCTL_MX | MADCTL_BGR);
    }

    #[test]
    fn fill_rect_sets_the_window_then_fills_the_pixel_count_in_one_transaction() {
        let mut lcd = core2();
        lcd.fill_rect(10, 20, 4, 3, color::RED).unwrap();
        assert_eq!(
            lcd.iface.log,
            std::vec![
                Op::Start,
                Op::Cmd(CMD_CASET),
                Op::Data(std::vec![0x00, 0x0A, 0x00, 0x0D]), // x0=10, x1=13
                Op::Cmd(CMD_PASET),
                Op::Data(std::vec![0x00, 0x14, 0x00, 0x16]), // y0=20, y1=22
                Op::Cmd(CMD_RAMWR),
                Op::Fill(color::RED, 12), // 4*3
                Op::End,
            ]
        );
    }

    #[test]
    fn a_window_address_is_big_endian_inclusive_and_end_equals_start_plus_size_minus_one() {
        let mut lcd = core2();
        lcd.fill_rect(0, 0, 320, 240, 0).unwrap();
        // x1 = 319 = 0x013F, y1 = 239 = 0x00EF.
        assert!(lcd.iface.log.contains(&Op::Data(std::vec![0x00, 0x00, 0x01, 0x3F])));
        assert!(lcd.iface.log.contains(&Op::Data(std::vec![0x00, 0x00, 0x00, 0xEF])));
        assert!(lcd.iface.log.contains(&Op::Fill(0, 320 * 240)));
    }

    #[test]
    fn a_draw_off_the_edge_is_rejected_before_any_bytes_move() {
        let mut lcd = core2();
        assert_eq!(lcd.fill_rect(300, 0, 40, 10, 0), Err(DisplayError::OutOfBounds));
        assert_eq!(lcd.fill_rect(0, 235, 10, 10, 0), Err(DisplayError::OutOfBounds));
        assert!(lcd.iface.log.is_empty(), "nothing was sent for a rejected draw");
    }

    #[test]
    fn a_blit_whose_buffer_does_not_match_the_region_is_rejected() {
        let mut lcd = core2();
        let px = [0u16; 5];
        assert_eq!(lcd.blit(0, 0, 2, 3, &px), Err(DisplayError::SizeMismatch)); // 2*3 != 5
        assert!(lcd.iface.log.is_empty());
    }

    #[test]
    fn a_matching_blit_streams_the_buffer_after_the_window() {
        let mut lcd = core2();
        let px = [color::BLUE; 6];
        lcd.blit(1, 1, 3, 2, &px).unwrap();
        assert_eq!(lcd.iface.log.first(), Some(&Op::Start));
        assert_eq!(lcd.iface.log.last(), Some(&Op::End));
        assert!(lcd.iface.log.contains(&Op::Cmd(CMD_RAMWR)));
        assert!(lcd.iface.log.contains(&Op::Pixels(6)));
    }

    #[test]
    fn init_pulses_reset_and_sends_the_wake_sequence() {
        let mut lcd = core2();
        let mut reset_calls = std::vec::Vec::new();
        let mut delays = 0u32;
        lcd.init(|asserted| reset_calls.push(asserted), |ms| delays += ms).unwrap();
        // Reset asserted then released.
        assert_eq!(reset_calls, std::vec![true, false]);
        // The wake path reached DISPON, with COLMOD=16bit and inversion on.
        assert!(lcd.iface.log.contains(&Op::Cmd(CMD_SLPOUT)));
        assert!(lcd.iface.log.contains(&Op::Data(std::vec![COLMOD_16BIT])));
        assert!(lcd.iface.log.contains(&Op::Cmd(CMD_INVON)));
        assert!(lcd.iface.log.contains(&Op::Cmd(CMD_DISPON)));
        assert!(delays > 0);
    }
}
