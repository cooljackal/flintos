#![no_std]

use flint_hal::bus::{BusConfig, BusError, BusResult, PhysicalBus, UartDataBits, UartParity, UartStopBits};

/// ESP32 UART physical driver.
/// Registers at `base_addr` (0x3FF40000 for UART0).
pub struct Esp32Uart {
    base: u32,
}

const UART_FIFO: u32 = 0x00;
const UART_INT_RAW: u32 = 0x04;
const UART_INT_ENA: u32 = 0x08;
const UART_CLKDIV: u32 = 0x10;
const UART_CONF0: u32 = 0x20;
const UART_CONF1: u32 = 0x24;
const UART_STATUS: u32 = 0x1C;

const UART_TXFIFO_EMPTY: u32 = 1 << 10;
const UART_TXFIFO_CNT_MASK: u32 = 0xFF << 16;
const UART_RXFIFO_CNT_MASK: u32 = 0xFF;
const UART_TXFIFO_RST: u32 = 1 << 6;
const UART_RXFIFO_RST: u32 = 1 << 7;

impl Esp32Uart {
    pub fn new(base_addr: u32) -> Self {
        Self { base: base_addr }
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    /// Write a single byte (polled).
    pub fn putc(&self, c: u8) {
        // Wait for TX FIFO to have space (< 128 bytes used).
        loop {
            let status = unsafe { self.reg(UART_STATUS).read_volatile() };
            let count = (status >> 16) & 0xFF;
            if count < 128 {
                break;
            }
        }
        unsafe {
            self.reg(UART_FIFO).write_volatile(c as u32);
        }
    }

    /// Read a single byte (polled).
    pub fn getc(&self) -> Option<u8> {
        let status = unsafe { self.reg(UART_STATUS).read_volatile() };
        let count = status & UART_RXFIFO_CNT_MASK;
        if count > 0 {
            let val = unsafe { self.reg(UART_FIFO).read_volatile() } as u8;
            Some(val)
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
}

impl PhysicalBus for Esp32Uart {
    fn init(&mut self, config: &BusConfig) -> BusResult<()> {
        match config {
            BusConfig::Uart { baud, data_bits, parity, stop_bits, tx, rx, .. } => {
                let regs = self.base;
                unsafe {
                    // Route UART0 signals to GPIO pins via IO MUX.
                    // IO MUX base: 0x3FF49000, each pin has one 32-bit register.
                    // Function 1 = U0TXD on GPIO tx, U0RXD on GPIO rx.
                    let iomux_base = 0x3FF49000u32;
                    core::ptr::write_volatile((iomux_base + *tx as u32 * 4) as *mut u32, 1u32);
                    core::ptr::write_volatile((iomux_base + *rx as u32 * 4) as *mut u32, 1u32);

                    // Reset FIFOs.
                    let conf0 = regs + UART_CONF0;
                    let mut val = conf0 as *mut u32;

                    // Set data bits.
                    let bit_count = match data_bits {
                        UartDataBits::Bits5 => 0,
                        UartDataBits::Bits6 => 1,
                        UartDataBits::Bits7 => 2,
                        UartDataBits::Bits8 => 3,
                    };
                    *val = (*val & !0x3) | bit_count;

                    // Set stop bits.
                    let stop = match stop_bits {
                        UartStopBits::Stop1 => 1,
                        UartStopBits::Stop1_5 => 2,
                        UartStopBits::Stop2 => 3,
                    };
                    *val = (*val & !(0x3 << 2)) | (stop << 2);

                    // Set parity.
                    match parity {
                        UartParity::None => *val &= !(1 << 4),
                        UartParity::Even => *val = (*val & !(1 << 4)) | (0 << 4),
                        UartParity::Odd => *val = (*val & !(1 << 4)) | (1 << 4),
                    }
                    let _ = val;

                    // Set baud rate.
                    // UART_CLKDIV = (APB_CLK / baud / 16)
                    // APB_CLK = 80 MHz
                    let apb_hz: u32 = 80_000_000;
                    let div = apb_hz / baud / 16;
                    (regs + UART_CLKDIV as u32) as *mut u32;
                    core::ptr::write_volatile((regs + UART_CLKDIV) as *mut u32, div);

                    // Enable TX and RX.
                    let conf1 = (regs + UART_CONF1) as *mut u32;
                    core::ptr::write_volatile(conf1, (1 << 4) | (1 << 6));

                    // Clear interrupts.
                    core::ptr::write_volatile((regs + UART_INT_RAW) as *mut u32, 0xFFFFFFFF);
                }
                Ok(())
            }
            _ => Err(BusError::InvalidConfig),
        }
    }

    fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        let len = tx.len().min(rx.len());
        for i in 0..len {
            self.putc(tx[i]);
            rx[i] = self.getc().unwrap_or(0);
        }
        Ok(())
    }

    fn set_enabled(&mut self, enabled: bool) {
        let conf0 = self.reg(UART_CONF0);
        unsafe {
            let mut val = conf0.read_volatile();
            if enabled {
                val |= 1 << 0; // UART_ENABLE
            } else {
                val &= !(1 << 0);
            }
            conf0.write_volatile(val);
        }
    }
}
