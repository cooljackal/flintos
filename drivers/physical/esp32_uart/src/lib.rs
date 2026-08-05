// SPDX-License-Identifier: Apache-2.0

#![no_std]

use flint_hal::bus::{BusConfig, BusError, BusResult, PhysicalBus, UartDataBits, UartParity, UartStopBits};

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

/// APB clock feeding the UART divider. Fixed at 80 MHz whenever the SoC runs
/// from the PLL, which is the only configuration Flint currently supports.
const APB_HZ: u32 = 80_000_000;

// ── Peripheral bases, used to identify which UART this instance drives ───────

const UART0_BASE: u32 = 0x3FF4_0000;
const UART1_BASE: u32 = 0x3FF5_0000;
const UART2_BASE: u32 = 0x3FF6_E000;

/// IO_MUX-native (tx, rx) pins for each UART. Driving a UART on any other pin
/// requires GPIO-matrix routing, which this driver does not yet implement.
fn native_pins(base: u32) -> Option<(u8, u8)> {
    match base {
        UART0_BASE => Some((1, 3)),
        UART1_BASE => Some((10, 9)),
        UART2_BASE => Some((17, 16)),
        _ => None,
    }
}

// ── IO_MUX ───────────────────────────────────────────────────────────────────

const IO_MUX_BASE: u32 = 0x3FF4_9000;
const IO_MUX_MCU_SEL_SHIFT: u32 = 12; // [14:12] alternate-function select
const IO_MUX_MCU_SEL_MASK: u32 = 0x7 << IO_MUX_MCU_SEL_SHIFT;
const IO_MUX_FUN_IE: u32 = 1 << 9; // input enable

/// IO_MUX register offset for a GPIO number.
///
/// The mapping is deliberately irregular in hardware -- the registers are
/// ordered by pad, not by GPIO number -- so a `pin * 4` computation lands on
/// the wrong register for almost every pin. This table is from the ESP32 TRM
/// IO_MUX register summary (matching `GPIO_PIN_MUX_REG` in esp-idf).
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

/// Select a pad's IO_MUX alternate function, preserving the other pad settings
/// (pull-ups, drive strength) that the bootloader may already have configured.
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

        // Route the pads. Only the IO_MUX-native pins are supported; anything
        // else needs GPIO-matrix routing, and reporting that honestly beats
        // configuring the peripheral onto pins it will never reach.
        match native_pins(self.base) {
            Some((native_tx, native_rx)) if *tx == native_tx && *rx == native_rx => unsafe {
                // MCU_SEL 0 selects the UART function on each of these pads.
                io_mux_select(*tx, 0, false)?;
                io_mux_select(*rx, 0, true)?;
            },
            _ => return Err(BusError::InvalidConfig),
        }

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

    fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        let len = tx.len().min(rx.len());
        for i in 0..len {
            self.putc(tx[i]);
            rx[i] = self.getc().unwrap_or(0);
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
    fn io_mux_offsets_are_not_linear_in_pin_number() {
        // The regression this guards: a `pin * 4` computation. GPIO1 lives at
        // 0x88, not 0x04, so any linear formula routes the console to the
        // wrong pad.
        assert_eq!(io_mux_offset(0), Some(0x44));
        assert_eq!(io_mux_offset(1), Some(0x88));
        assert_eq!(io_mux_offset(3), Some(0x84));
        assert_ne!(io_mux_offset(1), Some(1 * 4));
        assert_ne!(io_mux_offset(3), Some(3 * 4));
    }

    #[test]
    fn unbonded_pins_are_rejected() {
        for pin in 28..=31 {
            assert_eq!(io_mux_offset(pin), None, "GPIO{pin} is not bonded out");
        }
        assert_eq!(io_mux_offset(40), None);
    }

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
    fn native_pin_table_matches_the_uart_bases() {
        assert_eq!(native_pins(UART0_BASE), Some((1, 3)));
        assert_eq!(native_pins(UART1_BASE), Some((10, 9)));
        assert_eq!(native_pins(UART2_BASE), Some((17, 16)));
        assert_eq!(native_pins(0xDEAD_BEEF), None);
    }
}
