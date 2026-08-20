// SPDX-License-Identifier: Apache-2.0

#![no_std]

use hal::bus::{BusConfig, BusError, BusResult, PhysicalBus, UartDataBits, UartParity, UartStopBits};
use hal::pinmux::{PinConfig, PinMux, Signal};
use soc_esp32::addr;
use soc_esp32::{dport, poll, Esp32PinMux, APB_HZ};

/// ESP32 UART physical driver.
/// Registers at `base_addr` (0x3FF40000 for UART0).
pub struct Esp32Uart {
    base: u32,
}

// ── Register map ─────────────────────────────────────────────────────────────
//
// ESP32 TRM section 13.5 (UART register summary). Offsets from the peripheral
// base: UART0 0x3FF40000, UART1 0x3FF50000, UART2 0x3FF6E000.

const UART_FIFO: u32 = 0x00;
// Kept for completeness: a partial register map is how the previous revision
// ended up writing the baud divisor into UART_INT_CLR.
#[allow(dead_code)]
const UART_INT_RAW: u32 = 0x04;
#[allow(dead_code)]
const UART_INT_ST: u32 = 0x08;
const UART_INT_ENA: u32 = 0x0C;
const UART_INT_CLR: u32 = 0x10;
const UART_CLKDIV: u32 = 0x14;
const UART_STATUS: u32 = 0x1C;
const UART_CONF0: u32 = 0x20;
const UART_CONF1: u32 = 0x24;

// ── UART_STATUS_REG fields ───────────────────────────────────────────────────

const UART_RXFIFO_CNT_SHIFT: u32 = 0;
const UART_RXFIFO_CNT_MASK: u32 = 0xFF << UART_RXFIFO_CNT_SHIFT;
const UART_TXFIFO_CNT_SHIFT: u32 = 16;
const UART_TXFIFO_CNT_MASK: u32 = 0xFF << UART_TXFIFO_CNT_SHIFT;

/// The hardware TX FIFO is 128 bytes deep.
const UART_TXFIFO_DEPTH: u32 = 128;

// ── UART_CONF0_REG fields ────────────────────────────────────────────────────
//
// Bit positions matter here: an earlier revision wrote the data-bit count into
// bits [1:0], which are PARITY and PARITY_EN, silently enabling odd parity and
// leaving the word length at its reset value.

const CONF0_PARITY: u32 = 1 << 0; // 0 = even, 1 = odd (only meaningful with PARITY_EN)
const CONF0_PARITY_EN: u32 = 1 << 1;
const CONF0_BIT_NUM_SHIFT: u32 = 2; // [3:2]  0=5, 1=6, 2=7, 3=8 data bits
const CONF0_BIT_NUM_MASK: u32 = 0x3 << CONF0_BIT_NUM_SHIFT;
const CONF0_STOP_BIT_NUM_SHIFT: u32 = 4; // [5:4]  1=1, 2=1.5, 3=2 stop bits
const CONF0_STOP_BIT_NUM_MASK: u32 = 0x3 << CONF0_STOP_BIT_NUM_SHIFT;
const CONF0_RXFIFO_RST: u32 = 1 << 17;
const CONF0_TXFIFO_RST: u32 = 1 << 18;
/// Selects APB_CLK (rather than REF_TICK) as the baud-rate reference.
const CONF0_TICK_REF_ALWAYS_ON: u32 = 1 << 27;

// ── UART_CLKDIV_REG fields ───────────────────────────────────────────────────

const CLKDIV_INT_MASK: u32 = 0x000F_FFFF; // [19:0]  integer divisor
const CLKDIV_FRAG_SHIFT: u32 = 20; // [23:20] fractional divisor, in 1/16ths
const CLKDIV_FRAG_MASK: u32 = 0xF << CLKDIV_FRAG_SHIFT;


// ── Pin routing ──────────────────────────────────────────────────────────────
//
// Bases, native pads, and the IO_MUX offset table all live in
// `soc-esp32` now. They are chip facts, and this driver used to carry
// its own copy of them -- as did `esp32-spi`, separately, with a table that had
// drifted from this one.

/// Route TX and RX for controller `instance`.
///
/// Any pad will do: the GPIO matrix reaches them all, and `PinMux` takes the
/// IO_MUX direct path automatically when the requested pad happens to be the
/// native one. Before the SoC layer existed this driver accepted only the
/// native pair and rejected everything else.
fn route_pins(instance: u8, tx: u8, rx: u8) -> BusResult<()> {
    if tx == rx {
        return Err(BusError::InvalidConfig);
    }
    let mux = Esp32PinMux::new();
    let tx_sig = Signal::UartTx(instance);
    let rx_sig = Signal::UartRx(instance);

    // Validate both before routing either, so a bad manifest cannot leave the
    // console half-connected.
    mux.can_route(tx_sig, tx)?;
    mux.can_route(rx_sig, rx)?;

    mux.route(tx_sig, tx, PinConfig::PUSH_PULL)?;
    mux.route(rx_sig, rx, PinConfig::PUSH_PULL)?;
    Ok(())
}

impl Esp32Uart {
    /// Bind a driver instance to a UART peripheral.
    ///
    /// # Safety
    /// `base_addr` must be the base of a real ESP32 UART register block
    /// (`UART0_BASE`, `UART1_BASE`, or `UART2_BASE`), and the caller must own
    /// that peripheral exclusively -- two live instances on one base address
    /// race on the same registers with no synchronisation between them.
    pub unsafe fn new(base_addr: u32) -> Self {
        Self { base: base_addr }
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    /// Write a single byte (polled). Blocks until the TX FIFO has room.
    pub fn putc(&self, c: u8) {
        while self.tx_fifo_count() >= UART_TXFIFO_DEPTH {
            core::hint::spin_loop();
        }
        unsafe {
            self.reg(UART_FIFO).write_volatile(c as u32);
        }
    }

    /// Number of bytes currently queued in the TX FIFO.
    fn tx_fifo_count(&self) -> u32 {
        let status = unsafe { self.reg(UART_STATUS).read_volatile() };
        (status & UART_TXFIFO_CNT_MASK) >> UART_TXFIFO_CNT_SHIFT
    }

    /// Read a single byte (polled). `None` if the RX FIFO is empty.
    pub fn getc(&self) -> Option<u8> {
        let status = unsafe { self.reg(UART_STATUS).read_volatile() };
        let count = (status & UART_RXFIFO_CNT_MASK) >> UART_RXFIFO_CNT_SHIFT;
        if count > 0 {
            Some(unsafe { self.reg(UART_FIFO).read_volatile() } as u8)
        } else {
            None
        }
    }

    /// Write a byte string.
    pub fn write_str(&self, s: &[u8]) {
        for &b in s {
            self.putc(b);
        }
    }

    /// Block until every byte queued in the TX FIFO has been shifted out.
    ///
    /// Needed before a reset or a baud change, so in-flight output is not
    /// truncated or re-framed mid-character.
    pub fn flush(&self) {
        while self.tx_fifo_count() > 0 {
            core::hint::spin_loop();
        }
    }

    /// Program the baud-rate divider.
    ///
    /// The ESP32 divider is fractional: `baud = APB_CLK / (int + frag/16)`.
    /// An earlier revision divided by an additional 16, which put the port off
    /// by that factor.
    fn set_baud(&self, baud: u32) -> BusResult<()> {
        if baud == 0 || baud > APB_HZ {
            return Err(BusError::InvalidConfig);
        }
        let int_div = APB_HZ / baud;
        let frag = ((APB_HZ % baud) * 16) / baud;
        if int_div == 0 || int_div > CLKDIV_INT_MASK {
            return Err(BusError::InvalidConfig);
        }
        let val = (int_div & CLKDIV_INT_MASK) | ((frag << CLKDIV_FRAG_SHIFT) & CLKDIV_FRAG_MASK);
        unsafe { self.reg(UART_CLKDIV).write_volatile(val) };
        Ok(())
    }

    /// Reset both FIFOs. The reset bits are level-sensitive: they must be set
    /// and then cleared again, or the FIFOs stay held in reset.
    fn reset_fifos(&self) {
        unsafe {
            let conf0 = self.reg(UART_CONF0);
            let val = conf0.read_volatile();
            conf0.write_volatile(val | CONF0_RXFIFO_RST | CONF0_TXFIFO_RST);
            conf0.write_volatile(val & !(CONF0_RXFIFO_RST | CONF0_TXFIFO_RST));
        }
    }
}

impl PhysicalBus for Esp32Uart {
    fn init(&mut self, config: &BusConfig) -> BusResult<()> {
        let BusConfig::Uart { baud, data_bits, parity, stop_bits, tx, rx, .. } = config else {
            return Err(BusError::InvalidConfig);
        };

        let instance = addr::uart_instance(self.base).ok_or(BusError::InvalidConfig)?;

        // Clock and un-reset the peripheral before touching its registers.
        // UART0 works without this because the boot ROM has already ungated it
        // for its own console output -- but UART1 and UART2 come up gated off
        // and held in reset, and every write below would land nowhere with no
        // fault at all. The SPI and I2C drivers have always done this; this one
        // got away with not doing it because the console is the only port
        // anything has wired up so far.
        let clk_bit = dport::clock_bit(self.base).ok_or(BusError::InvalidConfig)?;
        unsafe { dport::enable(clk_bit) };

        route_pins(instance, *tx, *rx)?;

        // Drain anything the bootloader left queued before re-framing the port,
        // so the last bytes of its output are not corrupted mid-character.
        self.flush();

        self.set_baud(*baud)?;

        let bit_num: u32 = match data_bits {
            UartDataBits::Bits5 => 0,
            UartDataBits::Bits6 => 1,
            UartDataBits::Bits7 => 2,
            UartDataBits::Bits8 => 3,
        };
        let stop_num: u32 = match stop_bits {
            UartStopBits::Stop1 => 1,
            UartStopBits::Stop1_5 => 2,
            UartStopBits::Stop2 => 3,
        };

        unsafe {
            let conf0 = self.reg(UART_CONF0);
            let mut val = conf0.read_volatile();

            val &= !(CONF0_BIT_NUM_MASK | CONF0_STOP_BIT_NUM_MASK);
            val |= (bit_num << CONF0_BIT_NUM_SHIFT) & CONF0_BIT_NUM_MASK;
            val |= (stop_num << CONF0_STOP_BIT_NUM_SHIFT) & CONF0_STOP_BIT_NUM_MASK;

            val &= !(CONF0_PARITY | CONF0_PARITY_EN);
            match parity {
                UartParity::None => {}
                UartParity::Even => val |= CONF0_PARITY_EN,
                UartParity::Odd => val |= CONF0_PARITY_EN | CONF0_PARITY,
            }

            // Drive the divider from APB_CLK, matching the APB_HZ assumption
            // in set_baud.
            val |= CONF0_TICK_REF_ALWAYS_ON;

            conf0.write_volatile(val);

            // Interrupt-driven operation is not wired up yet: mask every source
            // and clear anything already latched, so a stale bit cannot raise a
            // spurious IRQ once the interrupt matrix is enabled.
            self.reg(UART_INT_ENA).write_volatile(0);
            self.reg(UART_INT_CLR).write_volatile(0xFFFF_FFFF);

            // RX threshold 1 byte, TX-empty threshold 32 bytes. Only consulted
            // once interrupts are enabled, but leaving CONF1 at reset would
            // give a 96-byte RX threshold, which stalls interactive input.
            self.reg(UART_CONF1).write_volatile(1 | (32 << 8));
        }

        self.reset_fifos();
        Ok(())
    }

    /// Full-duplex byte exchange.
    ///
    /// Write and read are separate phases per byte: `putc` only queues the byte
    /// in the TX FIFO, so the earlier `putc`-then-`getc` pairing read RX before
    /// the byte had shifted out and round-tripped — in a TX→RX loopback every
    /// `getc` saw an empty FIFO and returned zero, failing a working part.
    ///
    /// Now each byte is sent, then RX is polled (bounded) until it arrives. This
    /// keeps at most one byte in flight, so it never overruns the RX FIFO for an
    /// arbitrarily long transfer, and needs no per-byte sleep. A byte that never
    /// returns (nothing wired to RX) times out rather than hanging.
    fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        let len = tx.len().min(rx.len());
        for i in 0..len {
            self.putc(tx[i]);
            let mut byte = 0u8;
            poll::until(
                || match self.getc() {
                    Some(b) => {
                        byte = b;
                        true
                    }
                    None => false,
                },
                poll::DEFAULT_SPINS,
            )
            .map_err(|_| BusError::Timeout)?;
            rx[i] = byte;
        }
        Ok(())
    }

    /// No-op.
    ///
    /// The classic ESP32 UART has no enable bit -- the peripheral is live
    /// whenever it is clocked. Disabling it means gating `DPORT_PERIP_CLK_EN`,
    /// which would also cut the console this driver serves, so it is not done
    /// here. An earlier revision toggled CONF0 bit 0, which is the parity
    /// polarity bit, so "disabling" the port actually corrupted its framing.
    fn set_enabled(&mut self, _enabled: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conf0_fields_do_not_overlap() {
        // Data bits and stop bits occupy disjoint fields, and neither collides
        // with the parity bits. The original code wrote data bits into [1:0]
        // (parity) and stop bits into [3:2] (data bits).
        assert_eq!(CONF0_BIT_NUM_MASK, 0b1100);
        assert_eq!(CONF0_STOP_BIT_NUM_MASK, 0b11_0000);
        assert_eq!(CONF0_BIT_NUM_MASK & CONF0_STOP_BIT_NUM_MASK, 0);
        assert_eq!(CONF0_BIT_NUM_MASK & (CONF0_PARITY | CONF0_PARITY_EN), 0);
        assert_eq!(CONF0_STOP_BIT_NUM_MASK & (CONF0_PARITY | CONF0_PARITY_EN), 0);
    }

    #[test]
    fn baud_divisor_has_no_spurious_factor_of_16() {
        // 80 MHz / 115200 = 694.44 -> integer 694, fractional 7/16.
        let baud = 115_200u32;
        let int_div = APB_HZ / baud;
        let frag = ((APB_HZ % baud) * 16) / baud;
        assert_eq!(int_div, 694);
        assert_eq!(frag, 7);
        // The reconstructed rate must land within 1% of the request.
        let actual = (APB_HZ * 16) / (int_div * 16 + frag);
        let err = actual.abs_diff(baud) * 100 / baud;
        assert!(err < 1, "baud error {err}% too high (actual {actual})");
    }

    #[test]
    fn instance_lookup_covers_all_three_uarts() {
        use soc_esp32::addr::{UART0_BASE, UART1_BASE, UART2_BASE};
        assert_eq!(addr::uart_instance(UART0_BASE), Some(0));
        assert_eq!(addr::uart_instance(UART1_BASE), Some(1));
        assert_eq!(addr::uart_instance(UART2_BASE), Some(2));
        assert_eq!(addr::uart_instance(0xDEAD_BEEF), None);
    }

    #[test]
    fn a_uart_may_not_share_one_pad_for_tx_and_rx() {
        assert!(route_pins(0, 1, 1).is_err());
    }

    #[test]
    fn tx_cannot_land_on_an_input_only_pad() {
        // GPIO34-39 have no output driver, so the console would transmit into
        // nothing and look like a baud mismatch.
        assert!(route_pins(0, 34, 3).is_err());
        // RX on one of them is fine.
        let mux = Esp32PinMux::new();
        assert!(mux.can_route(Signal::UartRx(0), 34).is_ok());
    }
}
