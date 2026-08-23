// SPDX-License-Identifier: Apache-2.0

//! I2S0, driven as a DMA loopback.
//!
//! The classic ESP32's I2S has **no CPU-accessible FIFO** — data only moves
//! through the peripheral's own DMA. So even a loopback is a full DMA transfer:
//! a transmit chain feeds the serialiser, a receive chain drains the
//! deserialiser, and `sig_loopback` ties the transmitter's clock and data to
//! the receiver internally.
//!
//! One I2S block therefore tests its own whole data path — FIFO, serialiser,
//! deserialiser, and both DMA engines — with no pins and no second peripheral:
//! transmit a buffer, receive it back through the internal loopback, compare.
//!
//! The transmitter is **master** (it generates the bit and word clocks); the
//! receiver is **slave** and consumes those clocks over the loopback. Sixteen
//! bits per sample, two channels; the exact sample rate is immaterial when the
//! block only talks to itself.
//!
//! # One pad, for the data line
//!
//! `sig_loopback` shares the word and bit clocks between transmitter and
//! receiver internally, but **not the serial data** — verified on silicon,
//! where the receiver otherwise clocks in nothing but zeros. So the data output
//! and data input are routed to one pad through the GPIO matrix: the
//! transmitter drives it, the receiver reads it, no external wire. That pad is
//! the only pin this needs.
//!
//! Buffers and descriptors must live in DMA-reachable RAM and be word-aligned —
//! [`soc_esp32::dma::build_chain`] checks both. This crate reuses that tested
//! descriptor machinery rather than re-deriving the `lldesc` format.
//!
//! # Register facts
//!
//! `DR_REG_I2S_BASE` = `0x3FF4F000`, offsets from esp-idf `soc/i2s_reg.h`, field
//! positions from `soc/i2s_struct.h`, the sequence from `hal/i2s_ll.h`.
//!
//! | Register | Offset | Fields used |
//! |---|---|---|
//! | `CONF` | `0x08` | resets `[3:0]`, starts `[5:4]`, slave `[7:6]`, `MSB_SHIFT` `[11:10]`, `SIG_LOOPBACK` 18 |
//! | `INT_RAW`/`INT_CLR` | `0x0C`/`0x18` | `IN_SUC_EOF` 9 |
//! | `FIFO_CONF` | `0x20` | `DSCR_EN` 12, `TX_FIFO_MOD` `[15:13]`, `RX_FIFO_MOD` `[18:16]`, data-num `[11:0]` |
//! | `RXEOF_NUM` | `0x24` | words to receive before EOF |
//! | `CONF_CHAN` | `0x2C` | `TX_CHAN_MOD` `[2:0]`, `RX_CHAN_MOD` `[4:3]` |
//! | `OUT_LINK`/`IN_LINK` | `0x30`/`0x34` | `ADDR` `[19:0]`, `START` 29 |
//! | `LC_CONF` | `0x60` | in/out/AHBM resets `[3:0]` |
//! | `CLKM_CONF` | `0xAC` | `CLKM_DIV_NUM` `[7:0]`, `_B` `[13:8]`, `_A` `[19:14]`, `CLKA_ENA` 21 |
//! | `SAMPLE_RATE_CONF` | `0xB0` | bck-div `[11:0]`, bits-mod `[23:12]` |

#![no_std]

use hal::bus::BusResult;
use hal::pinmux::{PinConfig, PinMux, Signal};
use soc_esp32::dma::{
    build_chain, build_ring, link_addr, received_len, ring_slot, sync_for_device, Descriptor,
    Direction,
};
use soc_esp32::{dport, poll, reg, Esp32PinMux};

const I2S0_BASE: u32 = 0x3FF4_F000;

const CONF: u32 = I2S0_BASE + 0x08;
const INT_RAW: u32 = I2S0_BASE + 0x0C;
const INT_CLR: u32 = I2S0_BASE + 0x18;
const FIFO_CONF: u32 = I2S0_BASE + 0x20;
const RXEOF_NUM: u32 = I2S0_BASE + 0x24;
const CONF_CHAN: u32 = I2S0_BASE + 0x2C;
const OUT_LINK: u32 = I2S0_BASE + 0x30;
const IN_LINK: u32 = I2S0_BASE + 0x34;
const IN_EOF_DES_ADDR: u32 = I2S0_BASE + 0x3C;
const LC_CONF: u32 = I2S0_BASE + 0x60;
const CONF2: u32 = I2S0_BASE + 0xA8;
const CLKM_CONF: u32 = I2S0_BASE + 0xAC;
const SAMPLE_RATE_CONF: u32 = I2S0_BASE + 0xB0;

// CONF bits.
const CONF_TX_RESET: u32 = 1 << 0;
const CONF_RX_RESET: u32 = 1 << 1;
const CONF_TX_FIFO_RESET: u32 = 1 << 2;
const CONF_RX_FIFO_RESET: u32 = 1 << 3;
const CONF_TX_START: u32 = 1 << 4;
const CONF_RX_START: u32 = 1 << 5;
const CONF_RX_SLAVE_MOD: u32 = 1 << 7;
const CONF_TX_MSB_SHIFT: u32 = 1 << 10;
const CONF_RX_MSB_SHIFT: u32 = 1 << 11;
const CONF_SIG_LOOPBACK: u32 = 1 << 18;

// INT_RAW / INT_CLR: RX DMA finished a chain.
const INT_IN_SUC_EOF: u32 = 1 << 9;

// FIFO_CONF.
const FIFO_DSCR_EN: u32 = 1 << 12;
// The FIFO mode fields are only honoured with their force-enable bits set;
// without these the hardware picks a mode of its own and 16-bit data does not
// serialise the way the sample config asks for.
const FIFO_TX_MOD_FORCE_EN: u32 = 1 << 19;
const FIFO_RX_MOD_FORCE_EN: u32 = 1 << 20;
const FIFO_TX_DATA_NUM_SHIFT: u32 = 6;
const FIFO_RX_DATA_NUM_SHIFT: u32 = 0;
// 16-bit, two channels: TX and RX FIFO mode 0. The default FIFO threshold of
// 32 is left in the data-num fields.
const FIFO_DATA_NUM: u32 = 32;

// LC_CONF DMA resets.
const LC_IN_RST: u32 = 1 << 0;
const LC_OUT_RST: u32 = 1 << 1;
const LC_AHBM_FIFO_RST: u32 = 1 << 2;
const LC_AHBM_RST: u32 = 1 << 3;

// OUT_LINK / IN_LINK.
const LINK_STOP: u32 = 1 << 28;
const LINK_START: u32 = 1 << 29;

// CLKM_CONF: enable the clock module, no APLL (PLL_D2 = 160 MHz source),
// integer divide by 8. `CLK_EN` (bit 20) runs the clock generator at all —
// without it nothing serialises and a loopback DMA never completes.
const CLKM_CLK_EN: u32 = 1 << 20;
const CLKM_DIV_A_SHIFT: u32 = 14;
const CLKM_CONF_INT_DIV8: u32 = CLKM_CLK_EN | 8 | (1 << CLKM_DIV_A_SHIFT); // clk_en, div_num=8, div_a=1, div_b=0

// SAMPLE_RATE_CONF: bck divide by 8 on both, 16 bits per sample on both.
const RX_BCK_DIV_SHIFT: u32 = 6;
const TX_BITS_MOD_SHIFT: u32 = 12;
const RX_BITS_MOD_SHIFT: u32 = 18;
const SAMPLE_RATE_16BIT_BCK8: u32 =
    8 | (8 << RX_BCK_DIV_SHIFT) | (16 << TX_BITS_MOD_SHIFT) | (16 << RX_BITS_MOD_SHIFT);

/// Poll bound for the loopback. A few hundred samples at multi-MHz bit clock is
/// microseconds; this absorbs interrupts and still fails a stalled transfer.
const EOF_SPINS: u32 = 2_000_000;

/// Why a loopback failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I2sError {
    /// A buffer or descriptor was not DMA-reachable or not word-aligned.
    BadBuffer,
    /// The receive DMA never signalled end-of-frame.
    Timeout,
    /// The stream's buffers do not split into `count` equal, word-aligned
    /// pieces — the ring math needs `tx.len() == rx.len()`, both divisible by
    /// `count`, and each buffer a multiple of four bytes.
    BadGeometry,
}

/// I2S0, configured for an internal DMA loopback.
pub struct I2sLoopback {
    _private: (),
}

impl I2sLoopback {
    /// Bring I2S0 up: clocked, reset, master TX / slave RX, 16-bit, with the
    /// clock shared internally and the serial data looped over `data_pin`.
    ///
    /// `data_pin` must be a GPIO nothing else drives — the loopback is on that
    /// one pad.
    ///
    /// # Safety
    /// Takes exclusive ownership of the I2S0 registers, its DMA, and the pad.
    pub unsafe fn new(data_pin: u8) -> BusResult<Self> {
        dport::enable(dport::ClockBit::I2S0);

        // Loop the serial data through one pad: TX drives it, RX reads it. The
        // clocks are shared internally by sig_loopback below. TX first, then RX,
        // so the pad ends input-enabled.
        let mux = Esp32PinMux::new();
        mux.can_route(Signal::I2sTxData, data_pin)?;
        mux.can_route(Signal::I2sRxData, data_pin)?;
        mux.route(Signal::I2sTxData, data_pin, PinConfig::PUSH_PULL)?;
        mux.route(Signal::I2sRxData, data_pin, PinConfig::PUSH_PULL)?;

        // Reset the serialiser, deserialiser, both FIFOs, and the DMA.
        pulse(CONF, CONF_TX_RESET | CONF_RX_RESET | CONF_TX_FIFO_RESET | CONF_RX_FIFO_RESET);
        pulse(LC_CONF, LC_IN_RST | LC_OUT_RST | LC_AHBM_FIFO_RST | LC_AHBM_RST);

        // TX master, RX slave (it takes the looped-back clock); I2S-standard
        // MSB-shift on both; signal loopback on.
        write(
            CONF,
            CONF_RX_SLAVE_MOD | CONF_TX_MSB_SHIFT | CONF_RX_MSB_SHIFT | CONF_SIG_LOOPBACK,
        );

        // Two-channel (stereo) on both: TX_CHAN_MOD 0, RX_CHAN_MOD 0.
        write(CONF_CHAN, 0);

        write(CLKM_CONF, CLKM_CONF_INT_DIV8);
        // esp-idf's `i2s_ll_enable_clock` clears CONF2 when it starts the clock;
        // reset leaves it non-zero and the modes it selects fight this config.
        write(CONF2, 0);
        write(SAMPLE_RATE_CONF, SAMPLE_RATE_16BIT_BCK8);

        // DMA on, 16-bit dual-channel FIFO (mode 0) on both — forced, so the
        // mode fields are honoured — default threshold.
        write(
            FIFO_CONF,
            FIFO_DSCR_EN
                | FIFO_TX_MOD_FORCE_EN
                | FIFO_RX_MOD_FORCE_EN
                | (FIFO_DATA_NUM << FIFO_TX_DATA_NUM_SHIFT)
                | (FIFO_DATA_NUM << FIFO_RX_DATA_NUM_SHIFT),
        );

        Ok(Self { _private: () })
    }

    /// Transmit `tx` and receive it back through the internal loopback into
    /// `rx`. Returns the number of bytes received.
    ///
    /// `tx` and `rx` must be equal length, word-aligned, and DMA-reachable, as
    /// must `tx_descs`/`rx_descs`, which hold the DMA chains.
    ///
    /// # Safety
    /// Starts DMA against the caller's buffers; they must stay valid and
    /// untouched until this returns.
    pub unsafe fn loopback(
        &self,
        tx: &[u8],
        rx: &mut [u8],
        tx_descs: &mut [Descriptor],
        rx_descs: &mut [Descriptor],
    ) -> Result<usize, I2sError> {
        let len = tx.len() as u32;

        let tx_head = build_chain(tx_descs, tx.as_ptr() as u32, len, Direction::Transmit)
            .map_err(|_| I2sError::BadBuffer)?;
        let rx_head = build_chain(rx_descs, rx.as_mut_ptr() as u32, len, Direction::Receive)
            .map_err(|_| I2sError::BadBuffer)?;

        // Fresh FIFOs and DMA state for this transfer.
        pulse(CONF, CONF_TX_FIFO_RESET | CONF_RX_FIFO_RESET);
        pulse(LC_CONF, LC_IN_RST | LC_OUT_RST);

        // Receiver stops after this many 32-bit words — the ESP32 counts EOF in
        // words, not bytes.
        write(RXEOF_NUM, len / 4);
        write(INT_CLR, INT_IN_SUC_EOF);

        // Arm the receiver first, then start the transmitter, which generates
        // the clock the receiver is waiting on.
        write(IN_LINK, link_addr(rx_head) | LINK_START);
        set(CONF, CONF_RX_START);
        write(OUT_LINK, link_addr(tx_head) | LINK_START);
        set(CONF, CONF_TX_START);

        if poll::until(|| unsafe { read(INT_RAW) & INT_IN_SUC_EOF != 0 }, EOF_SPINS).is_err() {
            self.stop();
            return Err(I2sError::Timeout);
        }

        self.stop();
        Ok(received_len(rx_descs) as usize)
    }

    /// Halt both directions.
    unsafe fn stop(&self) {
        clear(CONF, CONF_TX_START | CONF_RX_START);
    }

    /// Start a **continuous** loopback stream: chain the buffers into a ring the
    /// DMA engine cycles forever, so the CPU can refill one buffer while the
    /// engine works the others — the underrun-free double-buffered pattern.
    ///
    /// `tx` and `rx` are each split into `count` equal buffers; they must be the
    /// same length, divisible by `count`, and each resulting buffer a multiple of
    /// four bytes. `tx_descs`/`rx_descs` must each hold at least `count`
    /// descriptors and, like the buffers, live in DMA-reachable RAM.
    ///
    /// Pre-fill `tx` before calling: the engine begins reading immediately.
    ///
    /// # Safety
    /// The returned [`I2sStream`] borrows the buffers and descriptors for the
    /// life of the stream; the engine reads and writes them until [`I2sStream::stop`].
    pub unsafe fn start_stream<'a>(
        &self,
        tx: &'a mut [u8],
        rx: &'a mut [u8],
        tx_descs: &'a mut [Descriptor],
        rx_descs: &'a mut [Descriptor],
        count: usize,
    ) -> Result<I2sStream<'a>, I2sError> {
        if count == 0 || tx.len() != rx.len() || tx.len() % count != 0 {
            return Err(I2sError::BadGeometry);
        }
        let chunk = tx.len() / count;
        if chunk == 0 || chunk % 4 != 0 {
            return Err(I2sError::BadGeometry);
        }

        let tx_head = build_ring(tx_descs, tx.as_ptr() as u32, count, chunk as u32, Direction::Transmit)
            .map_err(|_| I2sError::BadBuffer)?;
        let rx_head = build_ring(rx_descs, rx.as_mut_ptr() as u32, count, chunk as u32, Direction::Receive)
            .map_err(|_| I2sError::BadBuffer)?;

        // Fresh FIFOs and DMA state, exactly as the one-shot path does.
        pulse(CONF, CONF_TX_FIFO_RESET | CONF_RX_FIFO_RESET);
        pulse(LC_CONF, LC_IN_RST | LC_OUT_RST);

        // EOF every buffer, not every ring: the count is per-buffer words, and
        // each descriptor carries its own eof bit, so `IN_SUC_EOF` fires at each
        // buffer boundary — the tick the CPU services one buffer on.
        write(RXEOF_NUM, (chunk / 4) as u32);
        write(INT_CLR, INT_IN_SUC_EOF);

        // Publish the descriptor and buffer writes before the engine is pointed
        // at them (Xtensa write buffer; see soc::dma::sync_for_device).
        sync_for_device();

        // Arm the receiver first, then the transmitter that clocks it.
        write(IN_LINK, link_addr(rx_head) | LINK_START);
        set(CONF, CONF_RX_START);
        write(OUT_LINK, link_addr(tx_head) | LINK_START);
        set(CONF, CONF_TX_START);

        Ok(I2sStream { rx, rx_descs, tx, tx_descs, count, chunk, serviced: 0 })
    }

    /// True while either direction's start bit is set. A stopped stream must
    /// read false here; a self-test uses it to prove `stop` really stopped.
    ///
    /// # Safety
    /// Reads the I2S0 CONF register.
    pub unsafe fn is_running(&self) -> bool {
        read(CONF) & (CONF_TX_START | CONF_RX_START) != 0
    }

    /// Clear the buffer-boundary flag, then report whether the engine sets it
    /// again. After a clean stop it must stay clear — proof the DMA is not still
    /// cycling buffers behind a cleared start bit.
    ///
    /// # Safety
    /// Reads/writes the I2S0 interrupt registers.
    pub unsafe fn clear_eof(&self) {
        write(INT_CLR, INT_IN_SUC_EOF);
    }

    /// Whether a buffer-boundary EOF has been raised since [`Self::clear_eof`].
    ///
    /// # Safety
    /// Reads the I2S0 INT_RAW register.
    pub unsafe fn eof_pending(&self) -> bool {
        read(INT_RAW) & INT_IN_SUC_EOF != 0
    }
}

/// A running continuous DMA stream over the I2S loopback.
///
/// The descriptors form a ring the engine cycles endlessly; the CPU services one
/// buffer per `IN_SUC_EOF`. Between the EOF for a buffer and the engine lapping
/// back to it there are `count - 1` buffers of slack — the window in which the
/// CPU consumes the received data and refills the transmit buffer.
pub struct I2sStream<'a> {
    rx: &'a mut [u8],
    rx_descs: &'a mut [Descriptor],
    tx: &'a mut [u8],
    tx_descs: &'a mut [Descriptor],
    count: usize,
    chunk: usize,
    /// How many buffer-boundaries have been serviced. Only used to prove the
    /// stream is making progress; the engine does not need it.
    serviced: usize,
}

impl I2sStream<'_> {
    /// Block until the engine finishes another buffer, returning that buffer's
    /// ring index.
    ///
    /// The index comes from `IN_EOF_DES_ADDR`, the descriptor whose EOF fired,
    /// so a caller that falls a whole buffer behind sees the index jump rather
    /// than silently reading a stale buffer — which is what an under/overrun is.
    ///
    /// # Safety
    /// The engine is live against the buffers; the returned index is only safe
    /// to touch until the engine laps back to it (`count - 1` buffers later).
    pub unsafe fn wait(&mut self) -> Result<usize, I2sError> {
        if poll::until(|| unsafe { read(INT_RAW) & INT_IN_SUC_EOF != 0 }, EOF_SPINS).is_err() {
            return Err(I2sError::Timeout);
        }
        let eof_addr = read(IN_EOF_DES_ADDR);
        write(INT_CLR, INT_IN_SUC_EOF);
        self.serviced += 1;
        Ok(ring_slot(self.rx_descs.as_ptr() as u32, eof_addr) % self.count)
    }

    /// The received bytes of buffer `idx`.
    pub fn rx_buffer(&self, idx: usize) -> &[u8] {
        &self.rx[idx * self.chunk..(idx + 1) * self.chunk]
    }

    /// The transmit buffer `idx`, to refill before the engine laps back to it.
    pub fn tx_buffer_mut(&mut self, idx: usize) -> &mut [u8] {
        &mut self.tx[idx * self.chunk..(idx + 1) * self.chunk]
    }

    /// Publish CPU writes to a refilled transmit buffer before the engine reads
    /// it again. Call after [`Self::tx_buffer_mut`] edits, before the next
    /// [`Self::wait`].
    pub fn commit(&self) {
        sync_for_device();
    }

    /// How many buffer boundaries have been serviced so far.
    pub fn serviced(&self) -> usize {
        self.serviced
    }

    /// Stop the stream and leave the peripheral quiescent and restartable.
    ///
    /// Clears the start bits, stops both DMA links, and resets the serialisers,
    /// FIFOs and DMA — the same teardown the one-shot path relies on — then
    /// releases the CPU's owner claim on the descriptors so a stale engine-owned
    /// chain cannot be walked by a later transfer. Consumes the stream, which
    /// returns the borrowed buffers and descriptors to the caller for reuse.
    ///
    /// # Safety
    /// Writes the I2S0 registers. Any in-flight buffer is abandoned.
    pub unsafe fn stop(self) {
        // Halt the serialisers first, then the DMA links (esp-idf i2s_tx_stop /
        // i2s_rx_stop: stop the module, then stop_link, then reset).
        clear(CONF, CONF_TX_START | CONF_RX_START);
        set(OUT_LINK, LINK_STOP);
        set(IN_LINK, LINK_STOP);
        // Drain the peripheral back to reset: serialiser/deserialiser, both
        // FIFOs, and the DMA. Without this a restart re-arms on a half-full FIFO.
        pulse(CONF, CONF_TX_RESET | CONF_RX_RESET | CONF_TX_FIFO_RESET | CONF_RX_FIFO_RESET);
        pulse(LC_CONF, LC_IN_RST | LC_OUT_RST | LC_AHBM_FIFO_RST | LC_AHBM_RST);
        write(INT_CLR, INT_IN_SUC_EOF);
        // Hand the descriptors back to the CPU so the abandoned ring is not left
        // engine-owned for whatever reuses these slots next.
        for d in self.tx_descs.iter_mut().chain(self.rx_descs.iter_mut()) {
            core::ptr::write_volatile(d as *mut Descriptor, Descriptor::zeroed());
        }
        sync_for_device();
    }
}

// Thin address-based adapters over the shared, tested `soc_esp32::reg` helpers,
// so the read-modify-write logic lives in one place rather than being re-spelled
// here (the typo `& bits` for `& !bits` is what `reg` exists to prevent).
unsafe fn write(addr: u32, val: u32) {
    reg::write(addr as *mut u32, val);
}
unsafe fn read(addr: u32) -> u32 {
    reg::read(addr as *mut u32)
}
unsafe fn set(addr: u32, bits: u32) {
    reg::set(addr as *mut u32, bits);
}
unsafe fn clear(addr: u32, bits: u32) {
    reg::clear(addr as *mut u32, bits);
}
/// Set `bits`, then clear them: a reset strobe.
unsafe fn pulse(addr: u32, bits: u32) {
    set(addr, bits);
    clear(addr, bits);
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_addresses_match_i2s_reg_h() {
        assert_eq!(CONF, 0x3FF4_F008);
        assert_eq!(INT_RAW, 0x3FF4_F00C);
        assert_eq!(FIFO_CONF, 0x3FF4_F020);
        assert_eq!(RXEOF_NUM, 0x3FF4_F024);
        assert_eq!(CONF_CHAN, 0x3FF4_F02C);
        assert_eq!(OUT_LINK, 0x3FF4_F030);
        assert_eq!(IN_LINK, 0x3FF4_F034);
        assert_eq!(IN_EOF_DES_ADDR, 0x3FF4_F03C);
        assert_eq!(LC_CONF, 0x3FF4_F060);
        assert_eq!(CONF2, 0x3FF4_F0A8);
        assert_eq!(CLKM_CONF, 0x3FF4_F0AC);
        assert_eq!(SAMPLE_RATE_CONF, 0x3FF4_F0B0);
    }

    #[test]
    fn the_conf_bits_are_where_the_struct_orders_them() {
        // Off-by-one here silently swaps start for reset, or master for slave.
        assert_eq!(CONF_TX_START, 1 << 4);
        assert_eq!(CONF_RX_START, 1 << 5);
        assert_eq!(CONF_RX_SLAVE_MOD, 1 << 7);
        assert_eq!(CONF_TX_MSB_SHIFT, 1 << 10);
        assert_eq!(CONF_SIG_LOOPBACK, 1 << 18);
        // Loopback must not collide with the start or slave bits.
        assert_eq!(CONF_SIG_LOOPBACK & (CONF_TX_START | CONF_RX_SLAVE_MOD), 0);
    }

    #[test]
    fn the_rx_done_flag_is_bit_nine() {
        // IN_SUC_EOF: the receive DMA finished a chain. The whole loopback
        // waits on this bit; a wrong one waits forever or returns early.
        assert_eq!(INT_IN_SUC_EOF, 1 << 9);
    }

    #[test]
    fn the_clock_and_sample_fields_encode_16bit_by_8() {
        // The clock module must be enabled or nothing serialises and the
        // loopback DMA never completes — this was the first-silicon bug.
        assert_eq!(CLKM_CLK_EN, 1 << 20);
        assert_eq!(CLKM_CONF_INT_DIV8 & CLKM_CLK_EN, CLKM_CLK_EN);
        // div_num 8 in `[7:0]`, div_a 1 in `[19:14]`, div_b 0.
        assert_eq!(CLKM_CONF_INT_DIV8 & 0xFF, 8);
        assert_eq!((CLKM_CONF_INT_DIV8 >> 14) & 0x3F, 1);
        assert_eq!((CLKM_CONF_INT_DIV8 >> 8) & 0x3F, 0, "div_b must be zero for integer divide");
        // tx/rx bck div 8, tx/rx bits 16.
        assert_eq!(SAMPLE_RATE_16BIT_BCK8 & 0x3F, 8);
        assert_eq!((SAMPLE_RATE_16BIT_BCK8 >> RX_BCK_DIV_SHIFT) & 0x3F, 8);
        assert_eq!((SAMPLE_RATE_16BIT_BCK8 >> TX_BITS_MOD_SHIFT) & 0x3F, 16);
        assert_eq!((SAMPLE_RATE_16BIT_BCK8 >> RX_BITS_MOD_SHIFT) & 0x3F, 16);
    }

    #[test]
    fn dma_is_enabled_and_the_link_start_bit_is_twenty_nine() {
        assert_eq!(FIFO_DSCR_EN, 1 << 12);
        assert_eq!(FIFO_TX_MOD_FORCE_EN, 1 << 19);
        assert_eq!(FIFO_RX_MOD_FORCE_EN, 1 << 20);
        assert_eq!(LINK_START, 1 << 29);
        // Stop and start are distinct bits; the ring teardown sets stop.
        assert_eq!(LINK_STOP, 1 << 28);
        assert_eq!(LINK_STOP & LINK_START, 0);
        // The 20-bit address field must not reach the start or stop bits.
        assert_eq!(0x000F_FFFFu32 & (LINK_START | LINK_STOP), 0);
    }

    #[test]
    fn the_dma_reset_bits_are_the_low_four_of_lc_conf() {
        assert_eq!(LC_IN_RST, 1 << 0);
        assert_eq!(LC_OUT_RST, 1 << 1);
        assert_eq!(LC_AHBM_FIFO_RST, 1 << 2);
        assert_eq!(LC_AHBM_RST, 1 << 3);
    }
}
