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
//! | `CONF` | `0x08` | resets [3:0], starts [5:4], slave [7:6], `MSB_SHIFT` [11:10], `SIG_LOOPBACK` 18 |
//! | `INT_RAW`/`INT_CLR` | `0x0C`/`0x18` | `IN_SUC_EOF` 9 |
//! | `FIFO_CONF` | `0x20` | `DSCR_EN` 12, `TX_FIFO_MOD` [15:13], `RX_FIFO_MOD` [18:16], data-num [11:0] |
//! | `RXEOF_NUM` | `0x24` | words to receive before EOF |
//! | `CONF_CHAN` | `0x2C` | `TX_CHAN_MOD` [2:0], `RX_CHAN_MOD` [4:3] |
//! | `OUT_LINK`/`IN_LINK` | `0x30`/`0x34` | `ADDR` [19:0], `START` 29 |
//! | `LC_CONF` | `0x60` | in/out/AHBM resets [3:0] |
//! | `CLKM_CONF` | `0xAC` | `CLKM_DIV_NUM` [7:0], `_B` [13:8], `_A` [19:14], `CLKA_ENA` 21 |
//! | `SAMPLE_RATE_CONF` | `0xB0` | bck-div [11:0], bits-mod [23:12] |

#![no_std]

use hal::bus::BusResult;
use hal::pinmux::{PinConfig, PinMux, Signal};
use soc_esp32::dma::{build_chain, link_addr, received_len, Descriptor, Direction};
use soc_esp32::{dport, Esp32PinMux};

const I2S0_BASE: u32 = 0x3FF4_F000;

const CONF: u32 = I2S0_BASE + 0x08;
const INT_RAW: u32 = I2S0_BASE + 0x0C;
const INT_CLR: u32 = I2S0_BASE + 0x18;
const FIFO_CONF: u32 = I2S0_BASE + 0x20;
const RXEOF_NUM: u32 = I2S0_BASE + 0x24;
const CONF_CHAN: u32 = I2S0_BASE + 0x2C;
const OUT_LINK: u32 = I2S0_BASE + 0x30;
const IN_LINK: u32 = I2S0_BASE + 0x34;
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

        let mut spins = 0u32;
        while read(INT_RAW) & INT_IN_SUC_EOF == 0 {
            spins += 1;
            if spins > EOF_SPINS {
                self.stop();
                return Err(I2sError::Timeout);
            }
            core::hint::spin_loop();
        }

        self.stop();
        Ok(received_len(rx_descs) as usize)
    }

    /// Halt both directions.
    unsafe fn stop(&self) {
        clear(CONF, CONF_TX_START | CONF_RX_START);
    }
}

unsafe fn write(addr: u32, val: u32) {
    (addr as *mut u32).write_volatile(val);
}
unsafe fn read(addr: u32) -> u32 {
    (addr as *const u32).read_volatile()
}
unsafe fn set(addr: u32, bits: u32) {
    write(addr, read(addr) | bits);
}
unsafe fn clear(addr: u32, bits: u32) {
    write(addr, read(addr) & !bits);
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
        // div_num 8 in [7:0], div_a 1 in [19:14], div_b 0.
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
        // The 20-bit address field must not reach the start bit.
        assert_eq!(0x000F_FFFFu32 & LINK_START, 0);
    }

    #[test]
    fn the_dma_reset_bits_are_the_low_four_of_lc_conf() {
        assert_eq!(LC_IN_RST, 1 << 0);
        assert_eq!(LC_OUT_RST, 1 << 1);
        assert_eq!(LC_AHBM_FIFO_RST, 1 << 2);
        assert_eq!(LC_AHBM_RST, 1 << 3);
    }
}
