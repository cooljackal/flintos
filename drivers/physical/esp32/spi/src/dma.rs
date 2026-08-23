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

use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicU32, Ordering};

use hal::bus::{BusError, BusResult};
use hal::{DmaError, DmaHandle, DmaTransferId};
use soc_esp32::addr;
use soc_esp32::dma;
use soc_esp32::reg;

use super::{
    dma_slot, Esp32Spi, MAX_DESCS, SPI_CK_I_EDGE, SPI_CK_OUT_EDGE, SPI_CMD, SPI_CMD_USR,
    SPI_DMA, SPI_DOUTDIN, SPI_MISO_DLEN, SPI_MOSI_DLEN, SPI_SLAVE, SPI_SYNC_RESET,
    SPI_TIMEOUT_SPINS, SPI_TRANS_DONE, SPI_USER, SPI_USR_MISO, SPI_USR_MOSI,
};

// ── The kernel DMA broker, reached over the syscall ABI seam ─────────────────
//
// A physical driver may name only `hal` and the soc crates (tools/check-layers.sh),
// so it cannot depend on `kernel` or `api` to mint a transfer id or block on
// completion. It reaches the broker the same way `api::dma` does: the `extern
// "Rust"` symbols the kernel exports (`kernel::syscall`). This is the ABI seam,
// not a hidden crate dependency -- it is exactly parallel to the raw MMIO this
// whole driver is built on, which the layer check likewise cannot and does not
// forbid.
//
// The one broker call that stays in the caller is `signal_complete`: it runs in
// the interrupt top-half, whose handler is the application's (it names the
// `&'static` driver and the broker), so there is nothing for the driver to own.

/// Mint the completion id for a transfer over `handle`. Backs the id a
/// [`Transfer`] carries.
fn broker_begin(handle: &DmaHandle) -> Result<DmaTransferId, DmaError> {
    extern "Rust" {
        fn _flint_sys_dma_begin(handle: &DmaHandle) -> Result<DmaTransferId, DmaError>;
    }
    unsafe { _flint_sys_dma_begin(handle) }
}

/// Block the calling task until `id` completes or `timeout_ms` elapses.
fn broker_await(id: DmaTransferId, timeout_ms: u32) -> Result<(), DmaError> {
    extern "Rust" {
        fn _flint_sys_dma_await(id: DmaTransferId, timeout_ms: u32) -> Result<(), DmaError>;
    }
    unsafe { _flint_sys_dma_await(id, timeout_ms) }
}

/// How long [`Transfer::await_done`] waits before giving up. A full-length DMA
/// exchange at this bus's clocks completes in well under a millisecond; the
/// margin absorbs scheduling jitter while still failing a wedged engine.
const DMA_AWAIT_TIMEOUT_MS: u32 = 100;

/// The transfer in flight on each general-purpose host, for the top-half to
/// complete. Slot 0 is SPI2, slot 1 is SPI3; zero means "nothing in flight",
/// as [`DmaTransferId::from_raw`] documents. Set by [`Esp32Spi::exchange_async`]
/// before the engine starts and taken by the interrupt handler.
static SPI_DMA_PENDING: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

/// A DMA transfer started by [`Esp32Spi::exchange_async`], not yet awaited.
///
/// The engine is running; [`Transfer::await_done`] blocks the task until the
/// completion interrupt reports it done. Dropping it without awaiting leaves
/// the transfer to finish on its own — the interrupt still acknowledges the
/// peripheral — but the buffers must then stay valid, so awaiting is the norm.
#[must_use = "a started DMA transfer should be awaited"]
pub struct Transfer {
    id: DmaTransferId,
}

impl Transfer {
    /// Block until the transfer's completion interrupt fires, or the timeout
    /// passes.
    pub fn await_done(self) -> hal::Result<()> {
        broker_await(self.id, DMA_AWAIT_TIMEOUT_MS).map_err(Into::into)
    }

    /// The broker id this transfer will complete with, for a caller that stashes
    /// it elsewhere.
    pub fn id(&self) -> DmaTransferId {
        self.id
    }
}

const SPI_DMA_CONF: u32 = 0x100;
const SPI_DMA_OUT_LINK: u32 = 0x104;
const SPI_DMA_IN_LINK: u32 = 0x108;
const SPI_DMA_INT_ENA: u32 = 0x110;
const SPI_DMA_INT_RAW: u32 = 0x114;
const SPI_DMA_INT_CLR: u32 = 0x11C;
/// `SPI_DMA_RSTATUS`: transmit-DMA/FIFO status. Bit 31 is `TX_FIFO_EMPTY`.
const SPI_DMA_RSTATUS: u32 = 0x148;
const SPI_DMA_TX_FIFO_EMPTY: u32 = 1 << 31;

const SPI_IN_RST: u32 = 1 << 2;
const SPI_OUT_RST: u32 = 1 << 3;
const SPI_AHBM_FIFO_RST: u32 = 1 << 4;
const SPI_AHBM_RST: u32 = 1 << 5;

// Descriptor/data burst-enable bits in SPI_DMA_CONF. esp-idf enables these once
// in `spi_hal_init` (`spi_dma_ll_*_enable_burst_*`); the ESP32 has no RX *data*
// burst. Without descriptor burst the engine mis-fetches a re-armed chain, so a
// consecutive transfer moves zeros.
const SPI_OUT_EOF_MODE: u32 = 1 << 9;
const SPI_OUTDSCR_BURST_EN: u32 = 1 << 10;
const SPI_INDSCR_BURST_EN: u32 = 1 << 11;
const SPI_OUT_DATA_BURST_EN: u32 = 1 << 12;

// SPI_USER bits esp-idf leaves set that a full SPI_USER write must preserve: the
// MISO input-sampling edge and the chip-select hold. Confirmed against a live
// esp-idf run's register dump (USER = 0x18000051). SPI_CK_I_EDGE is shared, in
// the crate root.
const SPI_CS_HOLD: u32 = 1 << 4;

const SPI_OUTLINK_STOP: u32 = 1 << 28;
const SPI_OUTLINK_START: u32 = 1 << 29;
/// `SPI_INLINK_AUTO_RET`, SPI_DMA_IN_LINK bit 20: reset default 1. esp-idf's
/// armed IN_LINK keeps it set (its bitfield write preserves it); a full-register
/// write that omits it clears it, and a consecutive receive then misbehaves.
const SPI_INLINK_AUTO_RET: u32 = 1 << 20;
const SPI_INLINK_STOP: u32 = 1 << 28;
const SPI_INLINK_START: u32 = 1 << 29;

/// `SPI_IN_SUC_EOF_INT_RAW`: the receive chain hit end-of-frame.
pub const SPI_IN_SUC_EOF: u32 = 1 << 5;
/// `SPI_OUT_EOF_INT_RAW`: the transmit chain hit end-of-frame.
pub const SPI_OUT_EOF: u32 = 1 << 7;

// `SPI_TRANS_DONE` is shared, in the crate root. It is a *second* contributor
// to the same interrupt line as the DMA flags and easy to miss: `SPI_INT_EN`
// ([9:5]) has a reset default of `1_0000`, so it is enabled before anyone asks.
// Acknowledging the DMA flags alone leaves it asserted and the top-half
// re-enters forever, so `poll`/`ack` must clear it too.

impl Esp32Spi {
    /// Start a full-duplex DMA exchange and return without blocking.
    ///
    /// This owns everything a caller used to assemble by hand: it builds the
    /// transmit and receive descriptor chains in the host's own scratch (no
    /// caller-allocated descriptors), enables the completion interrupt at the
    /// peripheral, mints the broker id the interrupt will complete with, and
    /// fires the engine. [`Transfer::await_done`] then blocks the task until the
    /// interrupt reports the transfer done.
    ///
    /// `tx` and `rx` are broker buffers, so their memory is DMA-reachable by
    /// construction. Completion still needs an interrupt handler routed to this
    /// host's source (`interrupt::connect`) that acknowledges the peripheral
    /// ([`Esp32Spi::ack_interrupts`]) and hands the id from
    /// [`Esp32Spi::take_pending_dma`] to the broker's `signal_complete` — the
    /// top-half is the application's, because it names the kernel the driver may
    /// not.
    pub fn exchange_async(
        &self,
        tx: &DmaHandle,
        rx: &DmaHandle,
        len: usize,
    ) -> hal::Result<Transfer> {
        let instance = addr::spi_instance(self.base).ok_or(BusError::InvalidConfig)?;
        let s = dma_slot(instance).ok_or(BusError::InvalidConfig)?;

        if len == 0 || len as u32 > tx.size() || len as u32 > rx.size() {
            return Err(BusError::InvalidConfig.into());
        }
        let ndesc = dma::descriptors_needed(len as u32) as usize;
        if ndesc > MAX_DESCS {
            return Err(BusError::InvalidConfig.into());
        }

        // SAFETY: a bus is single-owner and a host's transfers are serialised,
        // so nothing else touches this slot while this runs.
        let slot = unsafe { &mut (*addr_of_mut!(SPI_DMA))[s] };
        if slot.channel.is_none() {
            return Err(BusError::DmaError.into());
        }

        let bank = slot.next_bank;
        slot.next_bank ^= 1;
        // SAFETY: descriptors sit in this slot's DMA-reachable scratch; the
        // broker buffers stay put until the transfer the caller awaits ends.
        let (tx_head, rx_head) = unsafe {
            let tx_head = dma::build_chain(
                &mut slot.tx_descs[bank][..ndesc],
                tx.addr(),
                len as u32,
                dma::Direction::Transmit,
            )
            .map_err(|_| BusError::DmaError)?;
            let rx_head = dma::build_chain(
                &mut slot.rx_descs[bank][..ndesc],
                rx.addr(),
                len as u32,
                dma::Direction::Receive,
            )
            .map_err(|_| BusError::DmaError)?;
            dma::sync_for_device();
            // Enable both end-of-frame sources before arming: the transmit
            // chain draining and the receive chain landing both reach the one
            // interrupt line, and the top-half acknowledges the pair.
            self.dma_int_enable(SPI_IN_SUC_EOF | SPI_OUT_EOF);
            (tx_head, rx_head)
        };

        // Mint the id and publish it before the engine starts: the transfer can
        // complete before the next instruction retires, and a top-half that
        // found no id would drop the completion.
        let id = broker_begin(rx).map_err(hal::Error::from)?;
        SPI_DMA_PENDING[s].store(id.raw(), Ordering::SeqCst);

        // SAFETY: the chains and their buffers stay valid until the caller
        // awaits the returned transfer.
        if let Err(e) = unsafe { self.start_dma(tx_head, rx_head, len) } {
            SPI_DMA_PENDING[s].store(0, Ordering::SeqCst);
            return Err(e.into());
        }
        Ok(Transfer { id })
    }

    /// Take the transfer id in flight on this host, clearing it. For the
    /// interrupt top-half: it hands the id to the broker's `signal_complete`.
    /// Returns `None` if nothing is pending.
    pub fn take_pending_dma(&self) -> Option<DmaTransferId> {
        let s = addr::spi_instance(self.base).and_then(dma_slot)?;
        match SPI_DMA_PENDING[s].swap(0, Ordering::SeqCst) {
            0 => None,
            raw => Some(DmaTransferId::from_raw(raw)),
        }
    }

    pub(crate) unsafe fn wait_dma_done(&self) -> BusResult<()> {
        // Completion is SPI_TRANS_DONE, exactly as esp-idf's `spi_hal_usr_is_done`
        // polls it. With SPI_INLINK_AUTO_RET set (as esp-idf leaves it), the
        // receive descriptor's length is not a reliable progress signal, so this
        // does not gate on it.
        let mut spins: u32 = 0;
        loop {
            let done = self.reg(SPI_SLAVE).read_volatile() & SPI_TRANS_DONE != 0;
            // The receive DMA raises IN_SUC_EOF once its last byte has landed in
            // memory — which trails the SPI transaction by a burst. Waiting on it
            // as well as TRANS_DONE is what stops the tail of `rx` coming back
            // short by the previous transfer's byte count.
            let int_raw = self.reg(SPI_DMA_INT_RAW).read_volatile();
            let rx_landed = int_raw & SPI_IN_SUC_EOF != 0;
            // The transmit DMA raises OUT_EOF when it has read the last byte of
            // the out chain. Without waiting for it the out engine can still be
            // draining when the next transfer pulses OUT_RST and re-arms the
            // out-link, and that transfer then clocks out zeros.
            let tx_drained = int_raw & SPI_OUT_EOF != 0;
            if done && rx_landed && tx_drained {
                return Ok(());
            }
            spins += 1;
            if spins > SPI_TIMEOUT_SPINS {
                self.dma_stop();
                return Err(BusError::Timeout);
            }
            core::hint::spin_loop();
        }
    }

    /// Put the host's DMA engine back to a known state.
    ///
    /// Both directions plus the AHB master. Skipping this inherits the
    /// previous transfer's descriptor position, which shows up as a transfer
    /// that returns the *previous* payload — the kind of bug that looks like
    /// an off-by-one in the caller.
    ///
    /// # Safety
    /// The host must be clocked and out of reset.
    /// Set up and fire one full-duplex DMA transaction, without waiting.
    ///
    /// This is esp-idf's exact ESP32-master order (`spi_hal_setup_trans` +
    /// `spi_hal_prepare_data` + `spi_hal_user_start`), which re-arms cleanly for
    /// consecutive transfers:
    ///
    /// 1. Clear the previous transaction's `TRANS_DONE` and DMA EOF flags.
    /// 2. Program the bit lengths (`spi_hal_setup_trans`).
    /// 3. Receiver: reset the RX DMA engine (`in_rst`), reset the shared AHB
    ///    FIFO (`AHBM_RST|AHBM_FIFO_RST`), then arm the in-link — each direction
    ///    reset immediately precedes its own link start.
    /// 4. Transmitter: the same for `out_rst` and the out-link.
    /// 5. Enable the MOSI/MISO data phases (esp-idf does this *after* the DMA is
    ///    armed for the ESP32), then start the transaction.
    ///
    /// # Safety
    /// Both chains and their buffers must stay valid until the transfer ends,
    /// and the host must own a DMA channel.
    unsafe fn dma_setup_and_fire(&self, tx_chain: u32, rx_chain: u32, len: usize) {
        let conf = self.reg(SPI_DMA_CONF);
        // NuttX's `esp32_spi_dma_exchange` per-transfer order, which is what
        // makes *consecutive* DMA transfers whole. The essential parts a DMA-only
        // reset misses: the links are disarmed first, and the SPI **core** FSM is
        // reset (SPI_SLAVE.sync_reset) as well as the DMA engine — the core reset
        // is what clears the residual receive state that otherwise drops the
        // first byte of every transfer after the first.

        // (1) Disarm both links before resetting anything.
        self.reg(SPI_DMA_IN_LINK).write_volatile(0);
        self.reg(SPI_DMA_OUT_LINK).write_volatile(0);

        // (2) Reset the SPI core transfer FSM, then the DMA engine (both
        // directions + AHB master/FIFO), with the links idle.
        let slave = self.reg(SPI_SLAVE);
        reg::set(slave, SPI_SYNC_RESET);
        reg::clear(slave, SPI_SYNC_RESET);
        let dma_rst = SPI_IN_RST | SPI_OUT_RST | SPI_AHBM_RST | SPI_AHBM_FIFO_RST;
        reg::set(conf, dma_rst);
        reg::clear(conf, dma_rst);

        // Descriptor/data burst — esp-idf enables these once at init; setting
        // them here (RMW, after the reset pulse) keeps the fetch mode right.
        reg::set(
            conf,
            SPI_OUT_EOF_MODE | SPI_INDSCR_BURST_EN | SPI_OUTDSCR_BURST_EN | SPI_OUT_DATA_BURST_EN,
        );

        // (3) Clear the prior transaction's completion flags.
        reg::clear(slave, SPI_TRANS_DONE);
        self.reg(SPI_DMA_INT_CLR)
            .write_volatile(SPI_IN_SUC_EOF | SPI_OUT_EOF);

        // (4) Bit lengths (bits − 1).
        let bits = (len as u32) * 8 - 1;
        self.reg(SPI_MOSI_DLEN).write_volatile(bits);
        self.reg(SPI_MISO_DLEN).write_volatile(bits);

        // (5) Arm the out-link (TX) then the in-link (RX).
        self.reg(SPI_DMA_OUT_LINK)
            .write_volatile(dma::link_addr(tx_chain) | SPI_OUTLINK_START);
        self.reg(SPI_DMA_IN_LINK)
            .write_volatile(dma::link_addr(rx_chain) | SPI_INLINK_AUTO_RET | SPI_INLINK_START);

        // (6) Enable the data phases. Match esp-idf's SPI_USER (0x18000051):
        // full-duplex, CS-hold, MISO-sampling edge, plus CPHA's clock-out edge.
        let ck_out = if self.cpha { SPI_CK_OUT_EDGE } else { 0 };
        self.reg(SPI_USER).write_volatile(
            SPI_DOUTDIN | SPI_CS_HOLD | SPI_CK_I_EDGE | SPI_USR_MOSI | SPI_USR_MISO | ck_out,
        );

        // (7) Wait until the out-DMA has primed the transmit FIFO before the
        // clock starts, so the first byte is present. NuttX polls this exact
        // status bit; without it a re-armed transfer clocks out its first byte
        // as zero.
        let mut spins: u32 = 0;
        while self.reg(SPI_DMA_RSTATUS).read_volatile() & SPI_DMA_TX_FIFO_EMPTY != 0 {
            spins += 1;
            if spins > SPI_TIMEOUT_SPINS {
                break;
            }
            core::hint::spin_loop();
        }

        self.reg(SPI_CMD).write_volatile(SPI_CMD_USR);
    }

    /// Run one full-duplex transfer of `len` bytes over DMA, polled to
    /// completion (`TRANS_DONE`).
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
        self.dma_setup_and_fire(tx_chain, rx_chain, len);
        self.wait_dma_done()
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
    /// Safe: it only clears this host's own interrupt flags, which owning the
    /// driver (`open`/`new`) entitles the caller to. Written to be callable
    /// straight from an interrupt top-half with no `unsafe` at the call site.
    pub fn ack_interrupts(&self) {
        // SAFETY: register writes to this host's own interrupt-clear and slave
        // registers; single ownership of the driver is what makes that sound.
        unsafe {
            self.reg(SPI_DMA_INT_CLR).write_volatile(u32::MAX);
            // Write-zero-to-clear, and only this bit: the rest of SPI_SLAVE_REG
            // holds mode configuration that a blind write would flatten.
            let slave = self.reg(SPI_SLAVE);
            reg::clear(slave, SPI_TRANS_DONE);
        }
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
        self.dma_setup_and_fire(tx_chain, rx_chain, len);
        Ok(())
    }

    /// Raw DMA end-of-frame flags, for a caller that wants to know *why* a
    /// transfer looks wrong.
    ///
    /// Safe: a read of this host's own status register, which owning the driver
    /// entitles the caller to. Callable from an interrupt top-half.
    pub fn dma_int_raw(&self) -> u32 {
        // SAFETY: a volatile read of this host's own register; single ownership
        // of the driver is what makes that sound.
        unsafe { self.reg(SPI_DMA_INT_RAW).read_volatile() }
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
