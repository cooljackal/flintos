// SPDX-License-Identifier: Apache-2.0

//! SSD1306 OLED display driver (128x64, I2C).
//!
//! Layer 3 logical driver — knows nothing about the bus or MCU.
//! Provides initialisation, clear, and temperature display.
//!
//! # Transport: I2C only
//!
//! The SSD1306 supports both I2C and SPI, but they are wired differently:
//! over I2C every payload byte is prefixed with a control byte (0x00 for a
//! command stream, 0x40 for a data/GRAM stream — see `Ssd1306::cmd` and
//! `write_data`); over SPI there is no control byte at all — the host
//! instead drives a dedicated D/C GPIO pin high or low around each byte.
//! [`BusHandle`] exposes no GPIO control, only [`BusHandle::select`]/
//! [`BusHandle::deselect`] and byte transfer, so there is no way
//! for this crate to toggle a D/C line even if it wanted to. Inlining the
//! I2C control byte into an SPI stream would corrupt every command, so
//! rather than pretend to support a transport this driver cannot correctly
//! drive, this doc is explicit: **this driver is I2C-only**. The board
//! manifest (`board/src/esp32_wrover.rs`) attaches the display to `i2c0`,
//! matching this.

#![no_std]
#![cfg_attr(not(test), forbid(unsafe_code))]
//
// The layer check reads the dependency graph, and raw MMIO needs no
// dependency -- a device driver could write 0x3FF44008 with `api` as its only
// dep and still pass. This is the line that makes "cannot reach hardware" true
// rather than aspirational.
//
// Scoped to non-test builds because the mock buses these crates test against
// use `unsafe` to extend a stack borrow to 'static (see `extend` below in
// bme280). That is test scaffolding and never ships; the shipping code in all
// three crates contains no `unsafe` at all.

use api::bus::{BusHandle, BusResult};

/// SSD1306 OLED display (128x64, I2C).
pub struct Ssd1306 {
    bus: BusHandle,
    width: u8,
    height: u8,
    pages: u8,
}

const CMD_CHARGE_PUMP: u8 = 0x8D;
const CMD_COM_SCAN_DEC: u8 = 0xC8;
const CMD_DISPLAY_OFF: u8 = 0xAE;
const CMD_DISPLAY_ON: u8 = 0xAF;
const CMD_DISPLAY_RAM: u8 = 0xA4;
const CMD_MEMORY_MODE: u8 = 0x20;
const CMD_NORMAL_DISPLAY: u8 = 0xA6;
const CMD_SEG_REMAP: u8 = 0xA1;
const CMD_SET_COL_ADDR: u8 = 0x21;
const CMD_SET_COM_PINS: u8 = 0xDA;
const CMD_SET_CONTRAST: u8 = 0x81;
const CMD_SET_DISPLAY_CLOCK: u8 = 0xD5;
const CMD_SET_DISPLAY_OFFSET: u8 = 0xD3;
const CMD_SET_MUX: u8 = 0xA8;
const CMD_SET_PAGE_ADDR: u8 = 0x22;
const CMD_SET_PRECHARGE: u8 = 0xD9;
const CMD_SET_VCOM_DETECT: u8 = 0xDB;
/// Set Display Start Line = 0. Encoded directly in the low 6 bits of the
/// command byte (0x40-0x7F), so unlike the other commands here it has no
/// separate data byte.
const CMD_SET_START_LINE_0: u8 = 0x40;

impl Ssd1306 {
    /// Create a new SSD1306 driver.
    pub fn new(bus: BusHandle) -> Self {
        Self { bus, width: 128, height: 64, pages: 8 }
    }

    /// Initialise the display.
    ///
    /// Verified against the SSD1306 datasheet's documented power-up
    /// sequence and cross-checked against the widely-used reference
    /// initialisation sequence for 128x64 internal-charge-pump modules
    /// (as shipped in, e.g., Adafruit's SSD1306 driver): display off,
    /// clock divider, multiplex ratio, display offset, start line, charge
    /// pump, addressing mode, segment remap, COM scan direction, COM pin
    /// configuration, contrast, pre-charge period, VCOMH deselect level,
    /// entire-display-on override off, normal (non-inverted) display, then
    /// clear the GRAM before finally powering the panel on — this avoids
    /// flashing stale/garbage RAM contents on first power-up. No
    /// discrepancies against the datasheet were found in this sequence;
    /// every command/value pair below is annotated with what it means.
    pub fn init(&self) -> BusResult<()> {
        self.cmd(CMD_DISPLAY_OFF)?;
        self.cmd(CMD_SET_DISPLAY_CLOCK)?;
        self.cmd(0x80)?; // default clock divide ratio / oscillator frequency
        self.cmd(CMD_SET_MUX)?;
        self.cmd(self.height - 1)?; // multiplex ratio = height - 1
        self.cmd(CMD_SET_DISPLAY_OFFSET)?;
        self.cmd(0x00)?; // no vertical shift
        self.cmd(CMD_SET_START_LINE_0)?;
        self.cmd(CMD_CHARGE_PUMP)?;
        self.cmd(0x14)?; // enable internal charge pump (module has no external Vcc)
        self.cmd(CMD_MEMORY_MODE)?;
        self.cmd(0x00)?; // horizontal addressing mode
        self.cmd(CMD_SEG_REMAP)?; // column 127 mapped to SEG0
        self.cmd(CMD_COM_SCAN_DEC)?; // COM scan direction remapped
        self.cmd(CMD_SET_COM_PINS)?;
        self.cmd(if self.height == 64 { 0x12 } else { 0x02 })?; // alternative COM pin config
        self.cmd(CMD_SET_CONTRAST)?;
        self.cmd(0xCF)?; // contrast for internal-charge-pump 128x64 panels
        self.cmd(CMD_SET_PRECHARGE)?;
        self.cmd(0xF1)?; // pre-charge period for internal Vcc
        self.cmd(CMD_SET_VCOM_DETECT)?;
        self.cmd(0x40)?; // VCOMH deselect level
        self.cmd(CMD_DISPLAY_RAM)?; // resume to RAM content display (entire-display-on off)
        self.cmd(CMD_NORMAL_DISPLAY)?;
        self.clear()?;
        self.cmd(CMD_DISPLAY_ON)?;
        Ok(())
    }

    /// Clear the display (fill with zeros).
    pub fn clear(&self) -> BusResult<()> {
        let total = self.width as u16 * self.pages as u16;
        self.cmd(CMD_SET_COL_ADDR)?;
        self.cmd(0)?;
        self.cmd(self.width - 1)?;
        self.cmd(CMD_SET_PAGE_ADDR)?;
        self.cmd(0)?;
        self.cmd(self.pages - 1)?;

        self.fill_data(0x00, total as usize)?;
        Ok(())
    }

    /// Send a command byte. I2C-only: prefixes the 0x00 "command stream"
    /// control byte — see the module-level docs for why this cannot also
    /// serve SPI.
    fn cmd(&self, byte: u8) -> BusResult<()> {
        self.bus.select()?;
        let result = self.bus.write(&[0x00, byte]);
        self.bus.deselect()?;
        result
    }

    /// Largest GRAM run this driver packs into one data transaction, over and
    /// above the leading 0x40 control byte. The buffers below are sized to it.
    const DATA_CHUNK: usize = 32;

    /// Per-transaction GRAM capacity: `DATA_CHUNK`, but never more than the
    /// bus's own limit minus the control byte.
    fn data_capacity(&self) -> usize {
        self.bus
            .max_transfer()
            .saturating_sub(1)
            .clamp(1, Self::DATA_CHUNK)
    }

    /// Write a run of GRAM bytes as `0x40` followed by up to a bus-full of data
    /// per transaction, instead of one transaction per byte.
    fn write_data(&self, bytes: &[u8]) -> BusResult<()> {
        let cap = self.data_capacity();
        let mut buf = [0x40u8; Self::DATA_CHUNK + 1];
        for run in bytes.chunks(cap) {
            buf[1..1 + run.len()].copy_from_slice(run);
            self.bus.select()?;
            let result = self.bus.write(&buf[..1 + run.len()]);
            self.bus.deselect()?;
            result?;
        }
        Ok(())
    }

    /// Write `count` copies of one GRAM byte, batched like [`write_data`] — the
    /// GRAM fill that `clear` needs without a `count`-sized buffer.
    fn fill_data(&self, byte: u8, count: usize) -> BusResult<()> {
        let cap = self.data_capacity();
        let mut buf = [0x40u8; Self::DATA_CHUNK + 1];
        buf[1..1 + cap].fill(byte);
        let mut remaining = count;
        while remaining > 0 {
            let n = remaining.min(cap);
            self.bus.select()?;
            let result = self.bus.write(&buf[..1 + n]);
            self.bus.deselect()?;
            result?;
            remaining -= n;
        }
        Ok(())
    }

    /// Render a temperature reading as text (e.g. "-12.3" or "25.9") on
    /// page 0 using a 5x7 font, rather than a length-coded bar. Values
    /// outside +/-99.9 are clamped rather than silently wrapped/truncated.
    pub fn print_temp(&self, temp_c: f32) -> BusResult<()> {
        self.clear()?;

        let (chars, len) = format_temp_chars(temp_c);

        // 5 columns per glyph plus 1 column of inter-glyph spacing, except
        // after the final glyph.
        let total_cols = (len as u16) * 6 - 1;
        let end_col = total_cols.min(self.width as u16) - 1;

        self.cmd(CMD_SET_PAGE_ADDR)?;
        self.cmd(0)?;
        self.cmd(0)?;
        self.cmd(CMD_SET_COL_ADDR)?;
        self.cmd(0)?;
        self.cmd(end_col as u8)?;

        // Build the row of glyph columns (5 per glyph + 1 spacing) and send it
        // in batched data transactions rather than one per column.
        let mut cols = [0u8; 6 * 6];
        let mut n = 0;
        for (i, &ch) in chars[..len].iter().enumerate() {
            for &col in &glyph_for(ch) {
                cols[n] = col;
                n += 1;
            }
            if i + 1 < len {
                cols[n] = 0x00; // inter-glyph spacing column
                n += 1;
            }
        }
        self.write_data(&cols[..n])?;
        Ok(())
    }
}

/// Format a temperature to one decimal place as ASCII characters, without
/// allocation or floating-point formatting (`no_std`, no `alloc`). Returns
/// the character buffer and the number of characters actually used.
fn format_temp_chars(temp_c: f32) -> ([u8; 6], usize) {
    let clamped = temp_c.clamp(-99.9, 99.9);
    // `f32::round` lives in `std`/`libm`, neither available here (`no_std`,
    // no `alloc`); round-half-away-from-zero by hand instead.
    let scaled = clamped * 10.0;
    let tenths = if scaled >= 0.0 { (scaled + 0.5) as i32 } else { (scaled - 0.5) as i32 };
    let negative = tenths < 0;
    let mag = tenths.unsigned_abs();
    let whole = mag / 10;
    let frac = mag % 10;

    let mut chars = [0u8; 6];
    let mut n = 0;
    if negative {
        chars[n] = b'-';
        n += 1;
    }
    if whole >= 10 {
        chars[n] = b'0' + (whole / 10) as u8;
        n += 1;
    }
    chars[n] = b'0' + (whole % 10) as u8;
    n += 1;
    chars[n] = b'.';
    n += 1;
    chars[n] = b'0' + frac as u8;
    n += 1;

    (chars, n)
}

/// Blank (all-zero) glyph used for any character outside the supported set.
const GLYPH_BLANK: [u8; 5] = [0x00, 0x00, 0x00, 0x00, 0x00];

/// Classic 5x7 font, digits 0-9 plus '-' and '.'. Each glyph is 5 columns;
/// each byte's bits 0-6 are that column's rows top-to-bottom (bit 7 unused,
/// row height 7 fits within a single 8-row SSD1306 page).
const GLYPH_DIGITS: [[u8; 5]; 10] = [
    [0x3E, 0x51, 0x49, 0x45, 0x3E], // 0
    [0x00, 0x42, 0x7F, 0x40, 0x00], // 1
    [0x42, 0x61, 0x51, 0x49, 0x46], // 2
    [0x21, 0x41, 0x45, 0x4B, 0x31], // 3
    [0x18, 0x14, 0x12, 0x7F, 0x10], // 4
    [0x27, 0x45, 0x45, 0x45, 0x39], // 5
    [0x3C, 0x4A, 0x49, 0x49, 0x30], // 6
    [0x01, 0x71, 0x09, 0x05, 0x03], // 7
    [0x36, 0x49, 0x49, 0x49, 0x36], // 8
    [0x06, 0x49, 0x49, 0x29, 0x1E], // 9
];
const GLYPH_MINUS: [u8; 5] = [0x08, 0x08, 0x08, 0x08, 0x08];
const GLYPH_DOT: [u8; 5] = [0x00, 0x60, 0x60, 0x00, 0x00];

fn glyph_for(ch: u8) -> [u8; 5] {
    match ch {
        b'0'..=b'9' => GLYPH_DIGITS[(ch - b'0') as usize],
        b'-' => GLYPH_MINUS,
        b'.' => GLYPH_DOT,
        _ => GLYPH_BLANK,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use api::bus::{Bus, BusResult, Op};
    use std::sync::Mutex;
    use std::vec::Vec;

    // `Bus` requires `Send + Sync`, so recorded writes use a `Mutex`
    // rather than a `RefCell` (which is not `Sync`). Each transaction is kept
    // whole: a command is `[0x00, cmd]` and a GRAM run is `[0x40, data..]`, and
    // batching means a data run is no longer a fixed two bytes.
    struct MockDisplayBus {
        transactions: Mutex<Vec<Vec<u8>>>,
    }

    impl Default for MockDisplayBus {
        fn default() -> Self {
            Self { transactions: Mutex::new(Vec::new()) }
        }
    }

    impl Bus for MockDisplayBus {
        fn transfer(&self, ops: &mut [Op]) -> BusResult<()> {
            for op in ops.iter_mut() {
                // The display is write-only; record each transaction whole.
                if let Some(tx) = op.tx {
                    self.transactions.lock().unwrap().push(tx.to_vec());
                }
            }
            Ok(())
        }
        fn max_transfer(&self) -> usize {
            64
        }
    }

    // Tests run single-threaded on the host and every handle built from
    // `extend` is dropped by the end of its test body, well within the
    // lifetime of the local `bus` it points at — this just satisfies
    // `BusHandle::new`'s `'static` bound without a heavier static-storage
    // pattern per test.
    unsafe fn extend<'a>(bus: &'a MockDisplayBus) -> &'static MockDisplayBus {
        core::mem::transmute::<&'a MockDisplayBus, &'static MockDisplayBus>(bus)
    }

    #[test]
    fn ssd1306_init_ok() {
        let bus = MockDisplayBus::default();
        let handle = BusHandle::new(unsafe { extend(&bus) });
        let display = Ssd1306::new(handle);
        assert!(display.init().is_ok());
    }

    #[test]
    fn ssd1306_init_command_sequence_matches_datasheet() {
        let bus = MockDisplayBus::default();
        let handle = BusHandle::new(unsafe { extend(&bus) });
        let display = Ssd1306::new(handle);
        assert!(display.init().is_ok());

        let txns = bus.transactions.lock().unwrap();
        // A command transaction is [0x00, cmd]; a GRAM run is [0x40, data..].
        // Pull out just the command stream (skip the GRAM-clear data, covered
        // by `ssd1306_clear`) and check the documented power-up sequence up
        // through charge-pump enable.
        let commands: Vec<u8> = txns
            .iter()
            .filter(|t| t.first() == Some(&0x00) && t.len() >= 2)
            .map(|t| t[1])
            .collect();
        assert_eq!(
            &commands[0..11],
            &[
                CMD_DISPLAY_OFF,
                CMD_SET_DISPLAY_CLOCK, 0x80,
                CMD_SET_MUX, 63, // height(64) - 1
                CMD_SET_DISPLAY_OFFSET, 0x00,
                CMD_SET_START_LINE_0,
                CMD_CHARGE_PUMP, 0x14,
                CMD_MEMORY_MODE,
            ]
        );
        // RAM display resume and normal display follow directly, then
        // `clear()` issues its own addressing commands before the final
        // display-on (issued last, after the GRAM clear, to avoid
        // flashing stale contents).
        let ram_pos = commands.iter().position(|&c| c == CMD_DISPLAY_RAM).unwrap();
        assert_eq!(commands[ram_pos + 1], CMD_NORMAL_DISPLAY);
        assert_eq!(*commands.last().unwrap(), CMD_DISPLAY_ON);
    }

    #[test]
    fn ssd1306_clear() {
        let bus = MockDisplayBus::default();
        let handle = BusHandle::new(unsafe { extend(&bus) });
        let display = Ssd1306::new(handle);
        assert!(display.clear().is_ok());
    }

    #[test]
    fn ssd1306_print_temp_positive() {
        let bus = MockDisplayBus::default();
        let handle = BusHandle::new(unsafe { extend(&bus) });
        let display = Ssd1306::new(handle);
        assert!(display.print_temp(25.5).is_ok());
    }

    #[test]
    fn ssd1306_print_temp_negative() {
        let bus = MockDisplayBus::default();
        let handle = BusHandle::new(unsafe { extend(&bus) });
        let display = Ssd1306::new(handle);
        assert!(display.print_temp(-12.3).is_ok());
    }

    #[test]
    fn format_temp_chars_positive() {
        let (chars, n) = format_temp_chars(25.4);
        assert_eq!(&chars[..n], b"25.4");
    }

    #[test]
    fn format_temp_chars_negative() {
        let (chars, n) = format_temp_chars(-12.3);
        assert_eq!(&chars[..n], b"-12.3");
    }

    #[test]
    fn format_temp_chars_distinguishes_nearby_values() {
        // The original bar-graph rendering truncated to an integer, so
        // 25.4 and 25.9 were indistinguishable on the display. They must
        // not format identically any more.
        let (a, na) = format_temp_chars(25.4);
        let (b, nb) = format_temp_chars(25.9);
        assert_ne!((&a[..na]), (&b[..nb]));
        assert_eq!(&a[..na], b"25.4");
        assert_eq!(&b[..nb], b"25.9");
    }

    #[test]
    fn format_temp_chars_does_not_saturate_negatives_to_zero() {
        // The original `as u8` cast saturated any negative temperature to
        // 0, rendering it identically to 0.0 degC.
        let (chars, n) = format_temp_chars(-5.0);
        assert_eq!(&chars[..n], b"-5.0");
        assert_ne!(&chars[..n], b"0.0".as_ref());
    }

    #[test]
    fn format_temp_chars_clamps_extreme_values() {
        let (chars, n) = format_temp_chars(250.0);
        assert_eq!(&chars[..n], b"99.9");
        let (chars, n) = format_temp_chars(-250.0);
        assert_eq!(&chars[..n], b"-99.9");
    }

    #[test]
    fn glyph_table_bounds() {
        // Every digit and the two punctuation glyphs must be non-blank and
        // exactly 5 columns; anything outside the supported set must fall
        // back to a blank glyph rather than panicking or indexing out of
        // bounds.
        for d in b'0'..=b'9' {
            let g = glyph_for(d);
            assert_eq!(g.len(), 5);
            assert_ne!(g, GLYPH_BLANK, "digit {} should not be blank", d as char);
        }
        assert_ne!(glyph_for(b'-'), GLYPH_BLANK);
        assert_ne!(glyph_for(b'.'), GLYPH_BLANK);
        assert_eq!(glyph_for(b'X'), GLYPH_BLANK);
        assert_eq!(glyph_for(0u8), GLYPH_BLANK);
        assert_eq!(glyph_for(255u8), GLYPH_BLANK);
    }
}
