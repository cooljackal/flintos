// SPDX-License-Identifier: Apache-2.0

#![no_std]

use flint_hal::bus::{BusConfig, BusError, BusResult, PhysicalBus, SpiMode};

/// ESP32 SPI2 (HSPI) / SPI3 (VSPI) physical driver (polled mode).
///
/// Bases: SPI2/HSPI 0x3FF64000, SPI3/VSPI 0x3FF65000.
pub struct Esp32Spi {
    base: u32,
}

// ── Register map ─────────────────────────────────────────────────────────────
//
// ESP32 TRM chapter 7 (SPI Controller), register summary; offsets confirmed
// against esp-idf `soc/spi_reg.h`. A prior revision had CLOCK/USER/USER1/
// PIN/SLAVE shifted down by one register slot (CLOCK=0x0C, USER=0x10,
// USER1=0x14, PIN=0x18, SLAVE=0x1C), which is really CTRL1/RD_STATUS/CTRL2/
// CLOCK/USER -- every one of those writes landed on the wrong register.

const SPI_CMD: u32 = 0x00;
#[allow(dead_code)] // Not needed for the byte-oriented polled transfer this driver implements.
const SPI_ADDR: u32 = 0x04;
#[allow(dead_code)]
const SPI_CTRL: u32 = 0x08;
#[allow(dead_code)] // Not needed for the byte-oriented polled transfer this driver implements.
const SPI_CTRL1: u32 = 0x0C;
#[allow(dead_code)]
const SPI_RD_STATUS: u32 = 0x10;
#[allow(dead_code)]
const SPI_CTRL2: u32 = 0x14;
const SPI_CLOCK: u32 = 0x18;
const SPI_USER: u32 = 0x1C;
#[allow(dead_code)] // Superseded by MOSI_DLEN/MISO_DLEN for byte-length transfers.
const SPI_USER1: u32 = 0x20;
#[allow(dead_code)]
const SPI_USER2: u32 = 0x24;
const SPI_MOSI_DLEN: u32 = 0x28;
const SPI_MISO_DLEN: u32 = 0x2C;
const SPI_PIN: u32 = 0x34;
const SPI_SLAVE: u32 = 0x38;
const SPI_W0: u32 = 0x80; // Data buffer: 16 words (W0..W15), 64 bytes.

/// SPI_CMD_REG: start a user-defined transaction. bitpos [18], confirmed
/// against esp-idf `soc/spi_reg.h` (`SPI_USR`). A prior revision wrote/polled
/// bit 0, which is `SPI_DOUTDIN` (a mode bit, not the start-transaction
/// strobe) -- the poll loop could spin forever since nothing ever clears it.
const SPI_CMD_USR: u32 = 1 << 18;

/// SPI_USER_REG bits (bitpos confirmed against esp-idf `soc/spi_reg.h`).
const SPI_USR_MISO: u32 = 1 << 28;
const SPI_USR_MOSI: u32 = 1 << 27;

/// Data buffer capacity: 16 32-bit words.
const SPI_DATA_BUF_WORDS: usize = 16;
const SPI_MAX_BYTES: usize = SPI_DATA_BUF_WORDS * 4;

/// Bound on `SPI_CMD_USR` poll iterations before giving up. A polled byte
/// transfer at the slowest supported clock completes in well under a
/// millisecond; this bound is generous enough to absorb scheduling jitter
/// while still failing a genuinely wedged peripheral instead of hanging
/// forever.
const SPI_TIMEOUT_SPINS: u32 = 1_000_000;

// ── DPORT peripheral clock/reset ─────────────────────────────────────────────
//
// SPI2 and SPI3 are clock-gated and held in reset at boot; every register
// access above is a no-op on real hardware until these are cleared. Bit
// positions confirmed against esp-idf `soc/dport_reg.h`
// (`DPORT_SPI2_CLK_EN`/`DPORT_SPI3_CLK_EN` and their `_RST` counterparts).

const DPORT_PERIP_CLK_EN_REG: u32 = 0x3FF0_00C0;
const DPORT_PERIP_RST_EN_REG: u32 = 0x3FF0_00C4;
const DPORT_SPI2_CLK_EN: u32 = 1 << 6;
const DPORT_SPI3_CLK_EN: u32 = 1 << 16;

const SPI2_BASE: u32 = 0x3FF6_4000;
const SPI3_BASE: u32 = 0x3FF6_5000;

/// DPORT clock/reset-enable bit for the SPI peripheral at `base`, if it is a
/// recognised base.
fn dport_clk_bit(base: u32) -> Option<u32> {
    match base {
        SPI2_BASE => Some(DPORT_SPI2_CLK_EN),
        SPI3_BASE => Some(DPORT_SPI3_CLK_EN),
        _ => None,
    }
}

// ── IO_MUX ───────────────────────────────────────────────────────────────────
//
// Shared source with `drivers/physical/esp32_uart/src/lib.rs::io_mux_offset`
// -- duplicated here rather than imported, per the layering rule that
// physical drivers don't depend on one another. Keep the two tables in sync
// if either is corrected.

const IO_MUX_BASE: u32 = 0x3FF4_9000;
const IO_MUX_MCU_SEL_SHIFT: u32 = 12; // [14:12] alternate-function select
const IO_MUX_MCU_SEL_MASK: u32 = 0x7 << IO_MUX_MCU_SEL_SHIFT;
const IO_MUX_FUN_IE: u32 = 1 << 9; // input enable

fn io_mux_offset(pin: u8) -> Option<u32> {
    let off: u32 = match pin {
        0 => 0x44,
        1 => 0x88,
        2 => 0x40,
        3 => 0x84,
        4 => 0x48,
        5 => 0x6C,
        6 => 0x60,
        7 => 0x64,
        8 => 0x68,
        9 => 0x54,
        10 => 0x58,
        11 => 0x5C,
        12 => 0x34,
        13 => 0x38,
        14 => 0x30,
        15 => 0x3C,
        16 => 0x4C,
        17 => 0x50,
        18 => 0x70,
        19 => 0x74,
        20 => 0x78,
        21 => 0x7C,
        22 => 0x80,
        23 => 0x8C,
        24 => 0x90,
        25 => 0x24,
        26 => 0x28,
        27 => 0x2C,
        32 => 0x1C,
        33 => 0x20,
        34 => 0x14,
        35 => 0x18,
        36 => 0x04,
        37 => 0x08,
        38 => 0x0C,
        39 => 0x10,
        _ => return None, // 28-31 are not bonded out on the ESP32
    };
    Some(off)
}

/// IO_MUX-native (mosi, miso, sck) pins for each SPI controller. Driving SPI
/// on any other pin requires GPIO-matrix routing, which this driver does not
/// implement -- see `drivers/physical/esp32_uart` for the pattern this
/// follows. Pin numbers confirmed against esp-idf `soc/io_mux_reg.h`
/// (HSPI: GPIO12/13/14 = MTDI/MTCK/MTMS; VSPI: GPIO18/19/23).
fn native_pins(base: u32) -> Option<(u8, u8, u8)> {
    match base {
        SPI2_BASE => Some((13, 12, 14)), // HSPI: MOSI=13, MISO=12, SCK=14
        SPI3_BASE => Some((23, 19, 18)), // VSPI: MOSI=23, MISO=19, SCK=18
        _ => None,
    }
}

/// Alternate-function select value for the native HSPI/VSPI signal on a pin.
/// Confirmed against esp-idf `soc/io_mux_reg.h`: e.g. `FUNC_MTDI_HSPIQ = 1`,
/// `FUNC_GPIO18_VSPICLK = 1`. (Function 2 on these pins is plain GPIO, not
/// SPI -- these pins double as JTAG signals and don't follow the "function 0
/// = GPIO" convention most pins use.)
const SPI_IO_MUX_FUNC: u32 = 1;

/// Select a pad's IO_MUX alternate function, preserving other pad settings.
///
/// # Safety
/// `pin` must be a valid ESP32 GPIO and the caller must own that pad.
unsafe fn io_mux_select(pin: u8, func: u32, input_enable: bool) -> BusResult<()> {
    let off = io_mux_offset(pin).ok_or(BusError::InvalidConfig)?;
    let reg = (IO_MUX_BASE + off) as *mut u32;
    let mut val = reg.read_volatile();
    val = (val & !IO_MUX_MCU_SEL_MASK) | ((func << IO_MUX_MCU_SEL_SHIFT) & IO_MUX_MCU_SEL_MASK);
    if input_enable {
        val |= IO_MUX_FUN_IE;
    }
    reg.write_volatile(val);
    Ok(())
}

/// Pack up to 4 bytes (little-endian: `bytes[0]` is the first byte
/// transmitted) into one `SPI_Wn` word. The ESP32 SPI data buffer packs 4
/// bytes per 32-bit word; a prior revision wrote/read one byte per word
/// (`word_addr` advancing 4 bytes per *byte* index), which both wasted 3 of
/// every 4 buffer words and misaligned every byte after the first.
fn pack_word(bytes: &[u8]) -> u32 {
    let mut word = 0u32;
    for (i, &b) in bytes.iter().take(4).enumerate() {
        word |= (b as u32) << (i * 8);
    }
    word
}

/// Inverse of `pack_word`: unpack up to 4 bytes from a `SPI_Wn` word into
/// `out`, honouring the default little-endian `SPI_WR_BYTE_ORDER`/
/// `SPI_RD_BYTE_ORDER` = 0 reset state (first byte transferred = LSB).
fn unpack_word(word: u32, out: &mut [u8]) {
    for (i, b) in out.iter_mut().take(4).enumerate() {
        *b = ((word >> (i * 8)) & 0xFF) as u8;
    }
}

impl Esp32Spi {
    /// Bind a driver instance to the SPI register block at `base_addr`.
    ///
    /// # Safety
    /// `base_addr` must be the base address of a real ESP32 SPI2 or SPI3
    /// register block (0x3FF64000 / 0x3FF65000) and must not be concurrently
    /// owned by another driver instance -- this type performs unchecked
    /// `read_volatile`/`write_volatile` at `base_addr + offset` with no
    /// further validation of the address itself.
    pub unsafe fn new(base_addr: u32) -> Self {
        Self { base: base_addr }
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    /// Perform a polled SPI transfer (up to 64 bytes).
    pub fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        let len = tx.len().min(rx.len()).min(SPI_MAX_BYTES);
        if len == 0 {
            return Ok(());
        }
        let nwords = len.div_ceil(4);

        unsafe {
            // Write TX data into the data buffer, 4 bytes per word.
            for w in 0..nwords {
                let start = w * 4;
                let end = (start + 4).min(len);
                let word = pack_word(&tx[start..end]);
                self.reg(SPI_W0 + (w as u32 * 4)).write_volatile(word);
            }

            // Bit-length fields, not byte count minus one: SPI_MOSI_DLEN /
            // SPI_MISO_DLEN hold (bits - 1). A prior revision wrote the bit
            // count into USER1, which holds an address-phase bit-length
            // field, not the data-phase length.
            let bits = (len as u32) * 8 - 1;
            self.reg(SPI_MOSI_DLEN).write_volatile(bits);
            self.reg(SPI_MISO_DLEN).write_volatile(bits);

            // Configure the transfer: MOSI + MISO phases, byte order left at
            // its little-endian reset default (bits 10/11 unset) to match
            // `pack_word`/`unpack_word`.
            self.reg(SPI_USER).write_volatile(SPI_USR_MOSI | SPI_USR_MISO);

            // Start the transfer (SPI_USR, bit 18 -- not bit 0).
            self.reg(SPI_CMD).write_volatile(SPI_CMD_USR);

            // Wait for completion, bounded: SPI_USR self-clears when the
            // hardware finishes the transaction.
            let mut spins: u32 = 0;
            while self.reg(SPI_CMD).read_volatile() & SPI_CMD_USR != 0 {
                spins += 1;
                if spins > SPI_TIMEOUT_SPINS {
                    return Err(BusError::Timeout);
                }
                core::hint::spin_loop();
            }

            // Read RX data back out, 4 bytes per word.
            for w in 0..nwords {
                let start = w * 4;
                let end = (start + 4).min(len);
                let word = self.reg(SPI_W0 + (w as u32 * 4)).read_volatile();
                unpack_word(word, &mut rx[start..end]);
            }
        }

        Ok(())
    }
}

impl PhysicalBus for Esp32Spi {
    fn init(&mut self, config: &BusConfig) -> BusResult<()> {
        match config {
            BusConfig::Spi { mosi, miso, sck, max_speed, mode } => {
                // Clock and un-reset the peripheral before touching any of
                // its registers -- SPI2/SPI3 are gated off and held in reset
                // at boot, so every access below would otherwise be a no-op.
                let clk_bit = dport_clk_bit(self.base).ok_or(BusError::InvalidConfig)?;
                unsafe {
                    let clk_en = DPORT_PERIP_CLK_EN_REG as *mut u32;
                    clk_en.write_volatile(clk_en.read_volatile() | clk_bit);
                    let rst_en = DPORT_PERIP_RST_EN_REG as *mut u32;
                    rst_en.write_volatile(rst_en.read_volatile() & !clk_bit);
                }

                // Route MOSI/MISO/SCK through IO_MUX. Only the native pin
                // triple is supported -- anything else needs GPIO-matrix
                // routing, and rejecting it honestly beats silently
                // configuring a peripheral onto pins it will never reach.
                let (native_mosi, native_miso, native_sck) =
                    native_pins(self.base).ok_or(BusError::InvalidConfig)?;
                if *mosi != native_mosi || *miso != native_miso || *sck != native_sck {
                    return Err(BusError::InvalidConfig);
                }
                unsafe {
                    io_mux_select(*mosi, SPI_IO_MUX_FUNC, false)?;
                    io_mux_select(*miso, SPI_IO_MUX_FUNC, true)?;
                    io_mux_select(*sck, SPI_IO_MUX_FUNC, false)?;
                }

                let apb_hz: u32 = 80_000_000;
                let speed_hz = max_speed.hz();
                let div = (apb_hz / speed_hz).max(2);

                unsafe {
                    // Clock configuration.
                    self.reg(SPI_CLOCK).write_volatile(
                        ((div / 2) << 12) |         // clkcnt_N
                        ((div / 2) << 6) |          // clkcnt_H
                        ((div - 1) & 0x3F)          // clkcnt_L
                    );

                    // SPI mode (CPOL, CPHA).
                    let (cpol, cpha) = match mode {
                        SpiMode::Mode0 => (0, 0),
                        SpiMode::Mode1 => (0, 1),
                        SpiMode::Mode2 => (1, 0),
                        SpiMode::Mode3 => (1, 1),
                    };

                    let mut pin = self.reg(SPI_PIN).read_volatile();
                    if cpol != 0 { pin |= 1 << 2; } else { pin &= !(1 << 2); }
                    if cpha != 0 { pin |= 1 << 1; } else { pin &= !(1 << 1); }
                    self.reg(SPI_PIN).write_volatile(pin);

                    // Enable master mode, disable slave.
                    let slave = self.reg(SPI_SLAVE);
                    slave.write_volatile(slave.read_volatile() & !1);
                }
                Ok(())
            }
            _ => Err(BusError::InvalidConfig),
        }
    }

    fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        self.transfer(tx, rx)
    }

    fn set_enabled(&mut self, _enabled: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_offsets_match_trm_spi_summary() {
        // Regression guard: the previous revision had CLOCK=0x0C, USER=0x10,
        // USER1=0x14, PIN=0x18, SLAVE=0x1C -- each one register slot early.
        assert_eq!(SPI_CMD, 0x00);
        assert_eq!(SPI_ADDR, 0x04);
        assert_eq!(SPI_CTRL, 0x08);
        assert_eq!(SPI_CTRL1, 0x0C);
        assert_eq!(SPI_RD_STATUS, 0x10);
        assert_eq!(SPI_CTRL2, 0x14);
        assert_eq!(SPI_CLOCK, 0x18);
        assert_eq!(SPI_USER, 0x1C);
        assert_eq!(SPI_USER1, 0x20);
        assert_eq!(SPI_USER2, 0x24);
        assert_eq!(SPI_MOSI_DLEN, 0x28);
        assert_eq!(SPI_MISO_DLEN, 0x2C);
        assert_eq!(SPI_PIN, 0x34);
        assert_eq!(SPI_SLAVE, 0x38);
        assert_eq!(SPI_W0, 0x80);
    }

    #[test]
    fn usr_start_bit_is_18_not_0() {
        // The core of the reported bug: writing/polling bit 0 (SPI_DOUTDIN)
        // instead of bit 18 (SPI_USR) never starts a real transaction and
        // can spin forever waiting for a bit nothing will clear.
        assert_eq!(SPI_CMD_USR, 1 << 18);
        assert_ne!(SPI_CMD_USR, 1);
    }

    #[test]
    fn pack_and_unpack_round_trip_four_bytes_per_word() {
        let bytes = [0xDE, 0xAD, 0xBE, 0xEF];
        let word = pack_word(&bytes);
        // Little-endian: first byte transmitted is the LSB.
        assert_eq!(word, 0xEFBE_ADDE);
        let mut out = [0u8; 4];
        unpack_word(word, &mut out);
        assert_eq!(out, bytes);
    }

    #[test]
    fn pack_handles_partial_final_word() {
        let bytes = [0x01, 0x02];
        let word = pack_word(&bytes);
        assert_eq!(word, 0x0000_0201);
    }

    #[test]
    fn dport_clk_bits_are_distinct_and_match_known_bases() {
        assert_eq!(dport_clk_bit(SPI2_BASE), Some(1 << 6));
        assert_eq!(dport_clk_bit(SPI3_BASE), Some(1 << 16));
        assert_ne!(dport_clk_bit(SPI2_BASE), dport_clk_bit(SPI3_BASE));
        assert_eq!(dport_clk_bit(0xDEAD_BEEF), None);
    }

    #[test]
    fn native_pin_table_matches_known_bases() {
        assert_eq!(native_pins(SPI2_BASE), Some((13, 12, 14)));
        assert_eq!(native_pins(SPI3_BASE), Some((23, 19, 18)));
        assert_eq!(native_pins(0xDEAD_BEEF), None);
    }

    #[test]
    fn io_mux_offsets_are_not_linear_in_pin_number() {
        assert_eq!(io_mux_offset(12), Some(0x34));
        assert_eq!(io_mux_offset(13), Some(0x38));
        assert_ne!(io_mux_offset(13), Some(13 * 4));
    }

    #[test]
    fn transfer_length_is_capped_at_the_data_buffer_size() {
        assert_eq!(SPI_MAX_BYTES, 64);
    }
}
