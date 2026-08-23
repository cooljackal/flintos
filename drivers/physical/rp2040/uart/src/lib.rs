// SPDX-License-Identifier: Apache-2.0

//! RP2040 UART driver for the chip's ARM PL011-compatible controllers.

#![no_std]

use hal::bus::{
    BusConfig, BusError, BusResult, BusSpeed, UartConfig, UartDataBits, UartParity, UartStopBits,
};
use hal::pinmux::{PinConfig, PinMux, PinPull, Signal};
use hal::stream::{ByteStream, StreamErrors};
use soc_rp2040::{
    enable_peripheral_clock, uart_instance, unreset, Rp2040PinMux, RESET_UART0, RESET_UART1,
    XOSC_HZ,
};

const DR: u32 = 0x00;
const RSR_ECR: u32 = 0x04;
const FR: u32 = 0x18;
const IBRD: u32 = 0x24;
const FBRD: u32 = 0x28;
const LCR_H: u32 = 0x2c;
const CR: u32 = 0x30;
const IMSC: u32 = 0x38;
const ICR: u32 = 0x44;
const FR_RXFE: u32 = 1 << 4;
const FR_TXFF: u32 = 1 << 5;
const CR_UARTEN: u32 = 1;
const CR_LBE: u32 = 1 << 7;
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;

pub struct Rp2040Uart {
    base: u32,
}

fn divisors(clock: u32, baud: u32) -> Option<(u16, u8)> {
    if baud == 0 {
        return None;
    }
    let scaled = (u64::from(clock) * 4 + u64::from(baud) / 2) / u64::from(baud);
    let integer = scaled / 64;
    if integer == 0 || integer > u64::from(u16::MAX) {
        return None;
    }
    Some((integer as u16, (scaled & 63) as u8))
}

impl Rp2040Uart {
    /// # Safety
    /// `base` must name an exclusively owned RP2040 UART register block.
    pub unsafe fn new(base: u32) -> Self {
        Self { base }
    }
    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    pub fn init(&mut self, config: &BusConfig) -> BusResult<()> {
        let BusConfig::Uart(UartConfig { tx, rx, baud, data_bits, parity, stop_bits }) = config
        else {
            return Err(BusError::InvalidConfig);
        };
        let instance = uart_instance(self.base).ok_or(BusError::InvalidConfig)?;
        if tx == rx || *stop_bits == UartStopBits::Stop1_5 {
            return Err(BusError::InvalidConfig);
        }
        let mux = Rp2040PinMux::new();
        mux.can_route(Signal::UartTx(instance), *tx)?;
        mux.can_route(Signal::UartRx(instance), *rx)?;
        let (ibrd, fbrd) = divisors(XOSC_HZ, *baud).ok_or(BusError::InvalidConfig)?;
        unsafe {
            // RESET_DONE is clocked by clk_peri. Waiting for it before
            // enabling that clock deadlocks with RESET already deasserted.
            enable_peripheral_clock();
            unreset(if instance == 0 {
                RESET_UART0
            } else {
                RESET_UART1
            });
            self.reg(CR).write_volatile(0);
            self.reg(IBRD).write_volatile(u32::from(ibrd));
            self.reg(FBRD).write_volatile(u32::from(fbrd));
            let width = match data_bits {
                UartDataBits::Bits5 => 0,
                UartDataBits::Bits6 => 1,
                UartDataBits::Bits7 => 2,
                UartDataBits::Bits8 => 3,
            };
            let mut lcr = (width << 5) | (1 << 4);
            if *stop_bits == UartStopBits::Stop2 {
                lcr |= 1 << 3;
            }
            match parity {
                UartParity::None => {}
                UartParity::Even => lcr |= (1 << 1) | (1 << 2),
                UartParity::Odd => lcr |= 1 << 1,
            }
            self.reg(LCR_H).write_volatile(lcr);
            self.reg(IMSC).write_volatile(0);
            self.reg(ICR).write_volatile(0x7ff);
        }
        mux.route(Signal::UartTx(instance), *tx, PinConfig::PUSH_PULL)?;
        // RX is an input: pull it up so the idle line reads high. Left floating
        // it latches a spurious start bit — a phantom 0x00 ahead of the real
        // data — the moment RXE is enabled below.
        let rx_config = PinConfig {
            pull: PinPull::Up,
            ..PinConfig::PUSH_PULL
        };
        mux.route(Signal::UartRx(instance), *rx, rx_config)?;
        unsafe { self.reg(CR).write_volatile(CR_UARTEN | CR_TXE | CR_RXE) };
        Ok(())
    }

    pub fn set_loopback(&self, enabled: bool) {
        unsafe {
            let value = self.reg(CR).read_volatile();
            self.reg(CR).write_volatile(if enabled {
                value | CR_LBE
            } else {
                value & !CR_LBE
            });
        }
    }

    pub fn set_speed(&self, speed: BusSpeed) -> BusResult<()> {
        let (integer, fraction) = divisors(XOSC_HZ, speed.hz()).ok_or(BusError::InvalidConfig)?;
        unsafe {
            self.reg(IBRD).write_volatile(u32::from(integer));
            self.reg(FBRD).write_volatile(u32::from(fraction));
            let line = self.reg(LCR_H).read_volatile();
            self.reg(LCR_H).write_volatile(line);
        }
        Ok(())
    }
}

unsafe impl Send for Rp2040Uart {}
unsafe impl Sync for Rp2040Uart {}

impl ByteStream for Rp2040Uart {
    fn write(&self, data: &[u8]) -> usize {
        let mut count = 0;
        for &byte in data {
            if unsafe { self.reg(FR).read_volatile() } & FR_TXFF != 0 {
                break;
            }
            unsafe { self.reg(DR).write_volatile(u32::from(byte)) };
            count += 1;
        }
        count
    }

    fn read(&self, data: &mut [u8]) -> usize {
        let mut count = 0;
        for byte in data {
            if unsafe { self.reg(FR).read_volatile() } & FR_RXFE != 0 {
                break;
            }
            *byte = unsafe { self.reg(DR).read_volatile() } as u8;
            count += 1;
        }
        count
    }

    fn errors(&self) -> StreamErrors {
        let status = unsafe { self.reg(RSR_ECR).read_volatile() };
        let errors = StreamErrors {
            framing: status & 1 != 0,
            parity: status & 2 != 0,
            overrun: status & 8 != 0,
        };
        if errors.any() {
            unsafe { self.reg(RSR_ECR).write_volatile(0xff) }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn divider_matches_pico_sdk_formula() {
        assert_eq!(divisors(12_000_000, 115_200), Some((6, 33)));
    }
    #[test]
    fn invalid_bauds_are_rejected() {
        assert_eq!(divisors(12_000_000, 0), None);
        assert_eq!(divisors(12_000_000, 12_000_001), None);
    }
    #[test]
    fn register_offsets_match_pl011_map() {
        assert_eq!(
            (DR, FR, IBRD, FBRD, LCR_H, CR),
            (0, 0x18, 0x24, 0x28, 0x2c, 0x30)
        );
    }
}
