// SPDX-License-Identifier: Apache-2.0

//! RP2040 chip facts used before peripheral drivers exist.
//!
//! Addresses and interrupt numbers are from the Raspberry Pi RP2040
//! datasheet, sections 2.2 (address map) and 2.3 (interrupts), build
//! 3184e8e (2025-02-20). The SRAM extent is the six-bank 264 KiB window.

#![no_std]

pub mod boot2;

pub const XIP_BASE: u32 = 0x1000_0000;
pub const XIP_SIZE: u32 = 16 * 1024 * 1024;
pub const SRAM_BASE: u32 = 0x2000_0000;
pub const SRAM_SIZE: u32 = 264 * 1024;
pub const SRAM_END: u32 = SRAM_BASE + SRAM_SIZE;

pub const IO_BANK0_BASE: u32 = 0x4001_4000;
pub const UART0_BASE: u32 = 0x4003_4000;
pub const SPI0_BASE: u32 = 0x4003_C000;
pub const I2C0_BASE: u32 = 0x4004_4000;
pub const SIO_BASE: u32 = 0xD000_0000;

pub const IRQ_IO_BANK0: u8 = 13;
pub const IRQ_SPI0: u8 = 18;
pub const IRQ_UART0: u8 = 20;
pub const IRQ_I2C0: u8 = 23;
pub const NVIC_IRQ_COUNT: u8 = 26;

pub struct Rp2040Dma;

impl hal::dma::DmaReach for Rp2040Dma {
    fn reachable(&self, addr: u32, len: u32) -> bool {
        if len == 0 {
            return true;
        }
        addr.checked_add(len - 1)
            .is_some_and(|last| addr >= SRAM_BASE && last < SRAM_END)
    }
}

pub struct Rp2040;

impl hal::soc::SystemOnChip for Rp2040 {
    type Dma = Rp2040Dma;

    const DMA: Self::Dma = Rp2040Dma;
    const DEFAULT_CPU_HZ: u32 = 125_000_000;
    const APB_HZ: u32 = 125_000_000;
    const CAPABILITIES: hal::soc::SocCapabilities = hal::soc::SocCapabilities {
        cores: 2,
        interrupt_matrix: false,
        cache_off_execution: false,
        hardware_rng: false,
    };

    unsafe fn configure_cpu_clock() {
        // The real clock setup belongs to the RP2040 boot implementation.
        // Until it exists, a target boot must not claim this method worked.
        #[cfg(target_arch = "arm")]
        panic!("RP2040 clock setup is not implemented")
    }

    unsafe fn reset_cause() -> u32 {
        0
    }

    fn reset_cause_name(_cause: u32) -> &'static str {
        "unknown"
    }

    fn measure_cpu_hz(_cycle_count: fn() -> Option<u32>) -> Option<u32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::{dma::DmaReach as _, soc::SystemOnChip as _};

    #[test]
    fn sram_is_exactly_six_documented_banks() {
        assert_eq!(SRAM_END, 0x2004_2000);
    }

    #[test]
    fn dma_requires_the_whole_range_to_be_in_sram() {
        assert!(Rp2040::DMA.reachable(SRAM_BASE, SRAM_SIZE));
        assert!(!Rp2040::DMA.reachable(SRAM_BASE - 1, 1));
        assert!(!Rp2040::DMA.reachable(SRAM_END - 1, 2));
        assert!(!Rp2040::DMA.reachable(u32::MAX - 1, 4));
    }

    #[test]
    fn peripheral_irqs_match_the_nvic_table() {
        assert_eq!(
            (IRQ_IO_BANK0, IRQ_SPI0, IRQ_UART0, IRQ_I2C0),
            (13, 18, 20, 23)
        );
    }
}
