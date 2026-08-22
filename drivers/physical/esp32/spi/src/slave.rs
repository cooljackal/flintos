// SPDX-License-Identifier: Apache-2.0

//! SPI **slave** mode. Included by [`crate`].
//!
//! The master driver in the parent module generates the clock; a slave does the
//! opposite — it sits waiting for an external master's clock and shifts data on
//! the edges the master provides. That inverts every direction: SCK, MOSI and CS
//! are *inputs*, MISO is the *output*, and there is no clock divider to program.
//!
//! # Buffer path, not DMA
//!
//! This uses the 64-byte data buffer (`SPI_W0..W15`), not a DMA descriptor
//! chain. esp-idf offers both for the slave, but the DMA slave path on the
//! classic ESP32 carries silicon quirks the reference works around only
//! partially (mode-0/2 clock-phase shims in `spi_ll_slave_set_mode`, an RX
//! channel that must be reset when CS rises mid-transfer — `spi_slave_hal_dma_
//! need_reset`), and none of that is checkable without hardware. The buffer path
//! is self-contained, correct for any transfer up to the buffer size, and is
//! esp-idf's own no-DMA slave path (`spi_slave_hal_prepare_data`, `use_dma ==
//! 0`). A DMA slave is a later, hardware-in-the-loop change.
//!
//! # Pins are the caller's
//!
//! [`Esp32SpiSlave::init`] touches registers only. It cannot route pins, because
//! the [`hal::pinmux::Signal`] variants fix a direction (`SpiSck` is an output)
//! and a slave needs them the other way round. The caller wires the slave's
//! inputs with `gpio_matrix::connect_input` and its MISO output with
//! `connect_output` directly — see `kernel::selftest_spi_slave`.
//!
//! # Register facts
//!
//! Offsets and bit positions from esp-idf `soc/spi_reg.h` and
//! `soc/spi_struct.h`; the slave sequence follows `spi_ll_slave_init`,
//! `spi_ll_slave_set_mode` and `spi_slave_hal_prepare_data` (no-DMA branch).

use hal::bus::{BusError, BusResult, SpiMode};
use soc_esp32::addr;
use soc_esp32::{dport, reg};

use super::{
    pack_word, unpack_word, SPI_CMD, SPI_CMD_USR, SPI_DOUTDIN, SPI_MAX_BYTES, SPI_SLAVE,
    SPI_TIMEOUT_SPINS, SPI_USER, SPI_USR_MISO, SPI_USR_MOSI, SPI_W0,
};
// Register offsets the master path leaves private; a child module may read an
// ancestor's private items, so these need no wider visibility.
use super::{SPI_CLOCK, SPI_CTRL, SPI_CTRL2, SPI_PIN};

// ── SPI_SLAVE_REG fields (offset 0x38) ───────────────────────────────────────
//
// Bit positions from `spi_struct.h`'s `slave` union: rd_buf_done[0] .. the
// interrupt-enable block, then wr_rd_buf_en[29], slave_mode[30], sync_reset[31].

/// `SPI_SLAVE_MODE`, bit 30: 1 selects slave, 0 master. The master path *clears*
/// this; this path sets it.
const SPI_SLAVE_MODE: u32 = 1 << 30;
/// `SPI_SLV_WR_RD_BUF_EN`, bit 29: enable the write/read buffer in slave mode.
/// esp-idf's `spi_ll_slave_init` sets it ("not sure if needed"); kept for parity.
const SPI_WR_RD_BUF_EN: u32 = 1 << 29;
/// `SPI_SYNC_RESET`, bit 31: reset the SPI clock/CS/data lines. Pulsed between
/// transactions so a slave that was left mid-shift starts each transfer clean.
const SPI_SYNC_RESET: u32 = 1 << 31;
/// `SPI_TRANS_DONE`, bit 4: the transaction finished. Write-zero-to-clear.
/// `spi_ll_usr_is_done` polls exactly this bit for the slave.
const SPI_TRANS_DONE: u32 = 1 << 4;

// ── SPI_USER_REG fields the slave needs beyond the shared ones ────────────────

/// `SPI_CK_I_EDGE`, `SPI_USER` bit 6: the slave's clock-input edge. It plays the
/// role `ck_out_edge` (bit 7) plays for the master and is combined with the
/// MISO/MOSI delay fields of `SPI_CTRL2`.
const SPI_CK_I_EDGE: u32 = 1 << 6;

// ── SPI_PIN_REG field ─────────────────────────────────────────────────────────

/// `SPI_CK_IDLE_EDGE`, `SPI_PIN` bit 29: the clock idle level (CPOL).
const SPI_CK_IDLE_EDGE: u32 = 1 << 29;

// ── Slave data-length registers ───────────────────────────────────────────────

/// `SPI_SLV_WRBUF_DLEN_REG`, offset 0x48: slave *output* (MISO) length, in bits
/// minus one, in [23:0].
const SPI_SLV_WRBUF_DLEN: u32 = 0x48;
/// `SPI_SLV_RDBUF_DLEN_REG`, offset 0x4C: slave *input* (MOSI) length, in bits
/// minus one, in [23:0].
const SPI_SLV_RDBUF_DLEN: u32 = 0x4C;

/// The `SPI_CTRL2` timing word for a slave in `mode`, plus the two clock-edge
/// bits that live elsewhere: `(ck_idle_edge, ck_i_edge, ctrl2)`.
///
/// Values are esp-idf's `spi_ll_slave_set_mode(mode, dma_used = false)`. The
/// MISO/MOSI delay fields compensate for the extra input latency of a
/// matrix-routed clock; getting them wrong samples MOSI on the wrong edge and
/// the first bit of every byte comes back shifted. `SPI_CTRL2`:
/// miso_delay_mode[17:16], miso_delay_num[20:18], mosi_delay_mode[22:21],
/// mosi_delay_num[25:23].
const fn slave_mode_timing(mode: SpiMode) -> (bool, bool, u32) {
    const fn ctrl2(miso_mode: u32, miso_num: u32, mosi_mode: u32, mosi_num: u32) -> u32 {
        (miso_mode << 16) | (miso_num << 18) | (mosi_mode << 21) | (mosi_num << 23)
    }
    match mode {
        // ck_idle_edge, ck_i_edge, ctrl2
        SpiMode::Mode0 => (true, false, ctrl2(0, 0, 2, 2)),
        SpiMode::Mode1 => (true, true, ctrl2(2, 0, 0, 0)),
        SpiMode::Mode2 => (false, true, ctrl2(0, 0, 1, 2)),
        SpiMode::Mode3 => (false, false, ctrl2(1, 0, 0, 0)),
    }
}

/// An ESP32 SPI2/SPI3 controller configured as a slave (buffer path, polled).
///
/// A lightweight handle over the register block, like [`super::Esp32Spi`]: it
/// carries only the base address and the mode's clock-input edge, so it can be
/// reconstructed freely.
pub struct Esp32SpiSlave {
    base: u32,
    /// The mode's `ck_i_edge` (`SPI_USER` bit 6), reapplied on every arm because
    /// arming rewrites the whole `SPI_USER` register.
    ck_i_edge: bool,
}

impl Esp32SpiSlave {
    /// Bind a slave to the SPI register block at `base_addr`.
    ///
    /// # Safety
    /// As [`super::Esp32Spi::new`]: `base_addr` must be a real SPI2/SPI3 block
    /// (0x3FF64000 / 0x3FF65000), singly owned.
    pub unsafe fn new(base_addr: u32) -> Self {
        Self { base: base_addr, ck_i_edge: false }
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    /// Configure the controller as a slave in `mode`. Registers only — the
    /// caller routes the pins (see the module docs).
    ///
    /// Follows esp-idf's `spi_ll_slave_init` then `spi_ll_slave_set_mode`.
    pub fn init(&mut self, mode: SpiMode) -> BusResult<()> {
        let instance = addr::spi_instance(self.base).ok_or(BusError::InvalidConfig)?;
        // SPI1 drives the boot flash; it is never a general-purpose slave.
        if instance == 1 {
            return Err(BusError::InvalidConfig);
        }

        // Clock and un-reset the block before touching its registers.
        let clk_bit = dport::clock_bit(self.base).ok_or(BusError::InvalidConfig)?;
        // SAFETY: enabling a peripheral clock is idempotent; the block is ours.
        unsafe { dport::enable(clk_bit) };

        // SAFETY: single-owner register block, clocked and out of reset above.
        unsafe {
            // spi_ll_slave_init: zero clock/user/ctrl, select slave mode with the
            // buffer enabled, then pulse the core reset with the config in place.
            self.reg(SPI_CLOCK).write_volatile(0);
            self.reg(SPI_USER).write_volatile(0);
            self.reg(SPI_CTRL).write_volatile(0);

            let slave = self.reg(SPI_SLAVE);
            slave.write_volatile(SPI_WR_RD_BUF_EN | SPI_SLAVE_MODE);
            reg::set(slave, SPI_SYNC_RESET);
            reg::clear(slave, SPI_SYNC_RESET);

            // Full duplex. usr_miso_highpart / usr_mosi_highpart stay 0 (whole
            // 64-byte buffer) from the SPI_USER = 0 above.
            reg::set(self.reg(SPI_USER), SPI_DOUTDIN);

            // spi_ll_slave_set_mode(mode, dma_used = false).
            let (ck_idle, ck_i, ctrl2) = slave_mode_timing(mode);
            self.ck_i_edge = ck_i;

            let pin = self.reg(SPI_PIN);
            if ck_idle {
                reg::set(pin, SPI_CK_IDLE_EDGE);
            } else {
                reg::clear(pin, SPI_CK_IDLE_EDGE);
            }
            if ck_i {
                reg::set(self.reg(SPI_USER), SPI_CK_I_EDGE);
            }
            self.reg(SPI_CTRL2).write_volatile(ctrl2);
        }

        Ok(())
    }

    /// Preload `tx` and arm the slave for a `len`-byte full-duplex exchange, then
    /// return without waiting.
    ///
    /// The slave must be armed *before* the master starts clocking, so arming and
    /// completion are split: a caller arms this, lets the master run, then calls
    /// [`Esp32SpiSlave::complete`]. `len` is capped at the 64-byte buffer.
    ///
    /// Follows `spi_slave_hal_prepare_data` (no-DMA branch) then
    /// `spi_slave_hal_user_start`.
    pub fn arm(&self, tx: &[u8], len: usize) -> BusResult<()> {
        let len = len.min(tx.len()).min(SPI_MAX_BYTES);
        let nwords = len.div_ceil(4);

        // SAFETY: single-owner register block, initialised as a slave.
        unsafe {
            // Reset the transfer FSM before loading the buffer, exactly as
            // esp-idf does — a slave left mid-shift would otherwise begin the
            // next transfer partway through a byte.
            let slave = self.reg(SPI_SLAVE);
            reg::set(slave, SPI_SYNC_RESET);
            reg::clear(slave, SPI_SYNC_RESET);

            // Preload the transmit data. The buffer is shared: as the master
            // clocks, received bytes overwrite these words, so `complete` reads
            // the reply from the same registers.
            for w in 0..nwords {
                let start = w * 4;
                let end = (start + 4).min(len);
                self.reg(SPI_W0 + (w as u32 * 4)).write_volatile(pack_word(&tx[start..end]));
            }

            // Lengths are (bits - 1). RDBUF is the input (MOSI) length, WRBUF the
            // output (MISO) length; for a full-duplex exchange they are equal.
            let bits = (len as u32) * 8 - 1;
            self.reg(SPI_SLV_RDBUF_DLEN).write_volatile(bits);
            self.reg(SPI_SLV_WRBUF_DLEN).write_volatile(bits);

            // Enable both data phases (full duplex), reasserting the mode's
            // clock-input edge that this whole-register write would otherwise
            // drop.
            let ck_i = if self.ck_i_edge { SPI_CK_I_EDGE } else { 0 };
            self.reg(SPI_USER)
                .write_volatile(SPI_DOUTDIN | ck_i | SPI_USR_MOSI | SPI_USR_MISO);

            // Clear the previous completion, then arm. For the slave, SPI_USR
            // does not start a transfer — it enables one that the master's clock
            // then drives.
            reg::clear(slave, SPI_TRANS_DONE);
            self.reg(SPI_CMD).write_volatile(SPI_CMD_USR);
        }

        Ok(())
    }

    /// Wait for the master to finish the armed transfer and read the `len` bytes
    /// it clocked in into `rx`.
    ///
    /// Completion is `SPI_TRANS_DONE`, the same bit `spi_ll_usr_is_done` polls.
    pub fn complete(&self, rx: &mut [u8], len: usize) -> BusResult<()> {
        let len = len.min(rx.len()).min(SPI_MAX_BYTES);
        let nwords = len.div_ceil(4);

        // SAFETY: single-owner register block, a transfer armed by `arm`.
        unsafe {
            let mut spins: u32 = 0;
            while self.reg(SPI_SLAVE).read_volatile() & SPI_TRANS_DONE == 0 {
                spins += 1;
                if spins > SPI_TIMEOUT_SPINS {
                    return Err(BusError::Timeout);
                }
                core::hint::spin_loop();
            }

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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slave_reg_bits_match_the_struct() {
        // From spi_struct.h `slave` union: the three that separate a slave from a
        // master, plus the completion bit the DMA path also names.
        assert_eq!(SPI_SLAVE_MODE, 1 << 30);
        assert_eq!(SPI_WR_RD_BUF_EN, 1 << 29);
        assert_eq!(SPI_SYNC_RESET, 1 << 31);
        assert_eq!(SPI_TRANS_DONE, 1 << 4);
    }

    #[test]
    fn slave_length_registers_are_where_the_reg_header_says() {
        assert_eq!(SPI_SLV_WRBUF_DLEN, 0x48);
        assert_eq!(SPI_SLV_RDBUF_DLEN, 0x4C);
    }

    #[test]
    fn ck_i_edge_is_bit_6_not_the_master_out_edge() {
        // The master encodes CPHA in ck_out_edge (bit 7); the slave uses
        // ck_i_edge (bit 6). Confusing the two samples on the wrong edge.
        assert_eq!(SPI_CK_I_EDGE, 1 << 6);
        assert_eq!(crate::SPI_CK_OUT_EDGE, 1 << 7);
    }

    #[test]
    fn mode0_timing_matches_esp_idf_no_dma() {
        // spi_ll_slave_set_mode(0, false): ck_idle_edge = 1, ck_i_edge = 0,
        // miso_delay(0,0), mosi_delay(2,2) -> ctrl2 = (2<<21)|(2<<23).
        let (ck_idle, ck_i, ctrl2) = slave_mode_timing(SpiMode::Mode0);
        assert!(ck_idle);
        assert!(!ck_i);
        assert_eq!(ctrl2, (2 << 21) | (2 << 23));
        assert_eq!(ctrl2, 0x0140_0000);
    }

    #[test]
    fn every_mode_packs_its_delay_fields_inside_ctrl2() {
        for mode in [SpiMode::Mode0, SpiMode::Mode1, SpiMode::Mode2, SpiMode::Mode3] {
            let (_, _, ctrl2) = slave_mode_timing(mode);
            // The delay fields occupy [25:16]; nothing must land elsewhere.
            assert_eq!(ctrl2 & !0x03FF_0000, 0, "{mode:?} set a bit outside the delay fields");
        }
    }

    #[test]
    fn modes_disagree_on_the_clock_edges() {
        // A quick guard that the table is not four copies of one row: the idle
        // and input edges together must distinguish the CPOL/CPHA pairs.
        let edges = |m| {
            let (idle, i, _) = slave_mode_timing(m);
            (idle, i)
        };
        assert_eq!(edges(SpiMode::Mode0), (true, false));
        assert_eq!(edges(SpiMode::Mode1), (true, true));
        assert_eq!(edges(SpiMode::Mode2), (false, true));
        assert_eq!(edges(SpiMode::Mode3), (false, false));
    }
}
