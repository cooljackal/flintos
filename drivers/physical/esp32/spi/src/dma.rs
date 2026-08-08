// SPDX-License-Identifier: Apache-2.0

//! SPI transfers driven by DMA. Included by [`crate`].
//!
//! The FIFO path in the parent module tops out at 64 bytes — the whole of
//! `SPI_W0..W15`. This path has no such limit: the engine walks a descriptor
//! chain built by `soc_esp32::dma` and the CPU is not in the loop.
//!
//! # What the caller has to have done first
//!
//! Three things, none of which this module can do on the caller's behalf:
//!
//! 1. **Claimed a channel** with `soc_esp32::dma::claim`. The crossbar is
//!    three channels shared between the SPI hosts, and a host with no channel
//!    selected transfers nothing while reporting success.
//! 2. **Built the chains** with `soc_esp32::dma::build_chain`, in memory from
//!    `kernel::dma_broker`. Descriptors are followed by hardware, so the chain
//!    is as much a DMA buffer as the data is.
//! 3. **Initialised the host** — clocks, pins, mode — through the ordinary
//!    `PhysicalBus::init`.
//!
//! # Register facts
//!
//! Offsets and bit positions from esp-idf `soc/spi_reg.h`:
//!
//! | Register | Offset | Fields used |
//! |---|---|---|
//! | `SPI_DMA_CONF` | `0x100` | `IN_RST` 2, `OUT_RST` 3, `AHBM_FIFO_RST` 4, `AHBM_RST` 5 |
//! | `SPI_DMA_OUT_LINK` | `0x104` | `ADDR` [19:0], `STOP` 28, `START` 29 |
//! | `SPI_DMA_IN_LINK` | `0x108` | `ADDR` [19:0], `STOP` 28, `START` 29 |
//! | `SPI_DMA_INT_RAW` | `0x114` | `IN_SUC_EOF` 5, `OUT_EOF` 7 |
//! | `SPI_DMA_INT_CLR` | `0x11C` | same bits |
//!
//! The link registers hold only **20 bits** of descriptor address. Writing the
//! full address sets the start and stop bits that share the register, which
//! starts a transfer nobody asked for — hence `dma::link_addr`.

use hal::bus::{BusError, BusResult};
use soc_esp32::dma;
use soc_esp32::reg;

use super::{Esp32Spi, SPI_CMD, SPI_CMD_USR, SPI_DOUTDIN, SPI_MISO_DLEN, SPI_MOSI_DLEN,
            SPI_SLAVE, SPI_TIMEOUT_SPINS, SPI_USER, SPI_USR_MISO, SPI_USR_MOSI};

const SPI_DMA_CONF: u32 = 0x100;
const SPI_DMA_OUT_LINK: u32 = 0x104;
const SPI_DMA_IN_LINK: u32 = 0x108;
const SPI_DMA_INT_ENA: u32 = 0x110;
const SPI_DMA_INT_RAW: u32 = 0x114;
const SPI_DMA_INT_CLR: u32 = 0x11C;

const SPI_IN_RST: u32 = 1 << 2;
const SPI_OUT_RST: u32 = 1 << 3;
const SPI_AHBM_FIFO_RST: u32 = 1 << 4;
const SPI_AHBM_RST: u32 = 1 << 5;

const SPI_OUTLINK_STOP: u32 = 1 << 28;
const SPI_OUTLINK_START: u32 = 1 << 29;
const SPI_INLINK_STOP: u32 = 1 << 28;
const SPI_INLINK_START: u32 = 1 << 29;

/// `SPI_IN_SUC_EOF_INT_RAW`: the receive chain hit end-of-frame.
pub const SPI_IN_SUC_EOF: u32 = 1 << 5;
/// `SPI_OUT_EOF_INT_RAW`: the transmit chain hit end-of-frame.
pub const SPI_OUT_EOF: u32 = 1 << 7;

/// `SPI_TRANS_DONE`, `SPI_SLAVE_REG` bit 4: the transaction finished.
///
/// A *second* contributor to the same interrupt line as the DMA flags, and the
/// one that is easy to miss — `SPI_INT_EN` ([9:5]) has a reset default of
/// `1_0000`, so this interrupt is **enabled before anyone asks for it**.
///
/// It is write-zero-to-clear, not write-one. Acknowledging the DMA flags alone
/// leaves this asserted, the peripheral keeps the line high, and the top-half
/// re-enters forever: the board goes silent partway through a transfer with no
/// fault and no panic.
const SPI_TRANS_DONE: u32 = 1 << 4;

impl Esp32Spi {
    /// Put the host's DMA engine back to a known state.
    ///
    /// Both directions plus the AHB master. Skipping this inherits the
    /// previous transfer's descriptor position, which shows up as a transfer
    /// that returns the *previous* payload — the kind of bug that looks like
    /// an off-by-one in the caller.
    ///
    /// # Safety
    /// The host must be clocked and out of reset.
    pub unsafe fn dma_reset(&self) {
        let conf = self.reg(SPI_DMA_CONF);
        let bits = SPI_IN_RST | SPI_OUT_RST | SPI_AHBM_FIFO_RST | SPI_AHBM_RST;
        reg::set(conf, bits);
        reg::clear(conf, bits);
        // Stale end-of-frame flags would make the very first poll succeed.
        self.reg(SPI_DMA_INT_CLR)
            .write_volatile(SPI_IN_SUC_EOF | SPI_OUT_EOF);
    }

    /// Run one full-duplex transfer of `len` bytes over DMA.
    ///
    /// `tx_chain` and `rx_chain` are head descriptor addresses from
    /// `soc_esp32::dma::build_chain`. Returns once the transaction has
    /// finished; the received length comes from `dma::received_len` on the
    /// receive chain, because a short read is normal and only the descriptors
    /// know about it.
    ///
    /// # Safety
    /// Both chains, and the buffers they point at, must stay valid and
    /// untouched until this returns. The host must own a DMA channel.
    pub unsafe fn transfer_dma(&self, tx_chain: u32, rx_chain: u32, len: usize) -> BusResult<()> {
        if len == 0 {
            return Ok(());
        }
        if !dma::reachable(tx_chain) || !dma::reachable(rx_chain) {
            return Err(BusError::InvalidConfig);
        }

        self.dma_reset();

        // Receive link first. Arming the transmitter before the receiver has
        // somewhere to put the bytes loses the leading edge of the frame on a
        // fast clock.
        self.reg(SPI_DMA_IN_LINK)
            .write_volatile(dma::link_addr(rx_chain) | SPI_INLINK_START);
        self.reg(SPI_DMA_OUT_LINK)
            .write_volatile(dma::link_addr(tx_chain) | SPI_OUTLINK_START);

        // Bit lengths, not byte counts, and biased by one — the same contract
        // the FIFO path uses.
        let bits = (len as u32) * 8 - 1;
        self.reg(SPI_MOSI_DLEN).write_volatile(bits);
        self.reg(SPI_MISO_DLEN).write_volatile(bits);

        self.reg(SPI_USER)
            .write_volatile(SPI_DOUTDIN | SPI_USR_MOSI | SPI_USR_MISO);
        self.reg(SPI_CMD).write_volatile(SPI_CMD_USR);

        let mut spins: u32 = 0;
        while self.reg(SPI_CMD).read_volatile() & SPI_CMD_USR != 0 {
            spins += 1;
            if spins > SPI_TIMEOUT_SPINS {
                self.dma_stop();
                return Err(BusError::Timeout);
            }
            core::hint::spin_loop();
        }
        Ok(())
    }

    /// Enable DMA completion interrupts for this host.
    ///
    /// The peripheral only *raises* the line. Getting it to a CPU still needs
    /// `soc_esp32::intr_map::route` and a handler — this crate cannot do
    /// either, because a physical driver may depend on `hal` and `soc/*` only
    /// and the crossbar is the kernel's to hand out. Enabling here and
    /// forgetting to route is a transfer whose interrupt never arrives, which
    /// looks exactly like a transfer that never finished.
    ///
    /// # Safety
    /// The host must be clocked.
    pub unsafe fn dma_int_enable(&self, mask: u32) {
        self.reg(SPI_DMA_INT_ENA).write_volatile(mask);
    }

    /// Acknowledge DMA interrupt flags. **A top-half must call this.**
    ///
    /// These are level-triggered at the peripheral. Returning from the handler
    /// without clearing them re-enters it immediately and forever, which
    /// presents as a board that boots and then goes silent.
    ///
    /// # Safety
    /// The host must be clocked.
    pub unsafe fn dma_int_clear(&self, mask: u32) {
        self.reg(SPI_DMA_INT_CLR).write_volatile(mask);
    }

    /// Acknowledge **everything** this host can be interrupting for.
    ///
    /// The DMA end-of-frame flags and `SPI_TRANS_DONE`, which share one
    /// interrupt source. A top-half that clears only the flags it was waiting
    /// for leaves the other asserted and never returns.
    ///
    /// # Safety
    /// The host must be clocked.
    pub unsafe fn ack_interrupts(&self) {
        self.reg(SPI_DMA_INT_CLR).write_volatile(u32::MAX);
        // Write-zero-to-clear, and only this bit: the rest of SPI_SLAVE_REG
        // holds mode configuration that a blind write would flatten.
        let slave = self.reg(SPI_SLAVE);
        reg::clear(slave, SPI_TRANS_DONE);
    }

    /// Arm both links and start the transaction, without waiting.
    ///
    /// The counterpart to [`Esp32Spi::transfer_dma`] for a caller that would
    /// rather block on an interrupt than spin. Completion is
    /// `SPI_IN_SUC_EOF` in [`Esp32Spi::dma_int_raw`], or the interrupt if one
    /// is routed.
    ///
    /// # Safety
    /// Both chains and their buffers must stay valid and untouched until the
    /// transfer completes. The host must own a DMA channel.
    pub unsafe fn start_dma(&self, tx_chain: u32, rx_chain: u32, len: usize) -> BusResult<()> {
        if len == 0 {
            return Ok(());
        }
        if !dma::reachable(tx_chain) || !dma::reachable(rx_chain) {
            return Err(BusError::InvalidConfig);
        }
        self.dma_reset();
        self.reg(SPI_DMA_IN_LINK)
            .write_volatile(dma::link_addr(rx_chain) | SPI_INLINK_START);
        self.reg(SPI_DMA_OUT_LINK)
            .write_volatile(dma::link_addr(tx_chain) | SPI_OUTLINK_START);
        let bits = (len as u32) * 8 - 1;
        self.reg(SPI_MOSI_DLEN).write_volatile(bits);
        self.reg(SPI_MISO_DLEN).write_volatile(bits);
        self.reg(SPI_USER)
            .write_volatile(SPI_DOUTDIN | SPI_USR_MOSI | SPI_USR_MISO);
        self.reg(SPI_CMD).write_volatile(SPI_CMD_USR);
        Ok(())
    }

    /// Raw DMA end-of-frame flags, for a caller that wants to know *why* a
    /// transfer looks wrong.
    ///
    /// # Safety
    /// The host must be clocked.
    pub unsafe fn dma_int_raw(&self) -> u32 {
        self.reg(SPI_DMA_INT_RAW).read_volatile()
    }

    /// Halt both links.
    ///
    /// # Safety
    /// The host must be clocked.
    pub unsafe fn dma_stop(&self) {
        self.reg(SPI_DMA_OUT_LINK).write_volatile(SPI_OUTLINK_STOP);
        self.reg(SPI_DMA_IN_LINK).write_volatile(SPI_INLINK_STOP);
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_register_offsets_match_spi_reg_h() {
        // `REG_SPI_BASE(i) + 0x100` and friends. An offset that is wrong by
        // one register writes the link address into the interrupt enable,
        // which enables nothing and starts nothing -- a transfer that silently
        // never happens.
        assert_eq!(SPI_DMA_CONF, 0x100);
        assert_eq!(SPI_DMA_OUT_LINK, 0x104);
        assert_eq!(SPI_DMA_IN_LINK, 0x108);
        assert_eq!(SPI_DMA_INT_ENA, 0x110);
        assert_eq!(SPI_DMA_INT_RAW, 0x114);
        assert_eq!(SPI_DMA_INT_CLR, 0x11C);
    }

    #[test]
    fn the_start_bit_does_not_collide_with_the_address_field() {
        // SPI_OUTLINK_ADDR is [19:0] and START is bit 29. If link_addr let
        // any high bit through, it would land on START/STOP/RESTART.
        let head = 0x3FFD_9000;
        let programmed = dma::link_addr(head) | SPI_OUTLINK_START;
        assert_eq!(programmed & 0x000F_FFFF, 0xD9000, "address was corrupted");
        assert_eq!(programmed & SPI_OUTLINK_START, SPI_OUTLINK_START);
        assert_eq!(programmed & SPI_OUTLINK_STOP, 0, "stop got set as well");
    }

    #[test]
    fn the_reset_bits_are_the_four_the_header_names() {
        assert_eq!(SPI_IN_RST, 1 << 2);
        assert_eq!(SPI_OUT_RST, 1 << 3);
        assert_eq!(SPI_AHBM_FIFO_RST, 1 << 4);
        assert_eq!(SPI_AHBM_RST, 1 << 5);
    }

    #[test]
    fn the_eof_flags_are_where_the_header_says() {
        assert_eq!(SPI_IN_SUC_EOF, 1 << 5);
        assert_eq!(SPI_OUT_EOF, 1 << 7);
    }
}
