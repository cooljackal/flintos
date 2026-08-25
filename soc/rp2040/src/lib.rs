// SPDX-License-Identifier: Apache-2.0

//! RP2040 chip facts used before peripheral drivers exist.
//!
//! Addresses and interrupt numbers are from the Raspberry Pi RP2040
//! datasheet, sections 2.2 (address map) and 2.3 (interrupts), build
//! 3184e8e (2025-02-20). The SRAM extent is the six-bank 264 KiB window.

#![no_std]

pub mod boot2;
pub mod multicore;
pub mod test_status;

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
#[cfg(target_arch = "arm")]
const TIMER_RAWL: *const u32 = 0x4005_4028 as *const u32;
#[cfg(target_arch = "arm")]
const CLOCKS_BASE: u32 = 0x4000_8000;
#[cfg(target_arch = "arm")]
const CLK_REF_CTRL: *mut u32 = (CLOCKS_BASE + 0x30) as *mut u32;
#[cfg(target_arch = "arm")]
const CLK_REF_SELECTED: *const u32 = (CLOCKS_BASE + 0x38) as *const u32;
#[cfg(target_arch = "arm")]
const CLK_SYS_CTRL: *mut u32 = (CLOCKS_BASE + 0x3c) as *mut u32;
#[cfg(target_arch = "arm")]
const CLK_SYS_SELECTED: *const u32 = (CLOCKS_BASE + 0x44) as *const u32;
#[cfg(target_arch = "arm")]
const XOSC_BASE: u32 = 0x4002_4000;
#[cfg(target_arch = "arm")]
const XOSC_CTRL: *mut u32 = XOSC_BASE as *mut u32;
#[cfg(target_arch = "arm")]
const XOSC_STATUS: *const u32 = (XOSC_BASE + 0x04) as *const u32;
#[cfg(target_arch = "arm")]
const XOSC_STARTUP: *mut u32 = (XOSC_BASE + 0x0c) as *mut u32;

#[cfg(target_arch = "arm")]
fn wait_for_bits(register: *const u32, mask: u32) -> bool {
    for _ in 0..1_000_000 {
        if unsafe { register.read_volatile() } & mask == mask {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Wio board crystal and the frequency selected for the first kernel boot.
pub const XOSC_HZ: u32 = 12_000_000;

pub const IRQ_IO_BANK0: u8 = 13;
pub const IRQ_SPI0: u8 = 18;
pub const IRQ_UART0: u8 = 20;
pub const IRQ_I2C0: u8 = 23;
pub const NVIC_IRQ_COUNT: u8 = 26;

/// RP2040 free-running microsecond timer, independent of SysTick and PRIMASK.
#[cfg(target_arch = "arm")]
pub fn timer_us() -> u32 {
    unsafe { TIMER_RAWL.read_volatile() }
}

pub struct Rp2040Dma;

impl hal::dma::DmaReach for Rp2040Dma {
    fn reachable(&self, addr: u32, len: u32) -> bool {
        hal::dma::range_within(addr, len, SRAM_BASE, SRAM_END)
    }
}

pub struct Rp2040;

impl hal::soc::SystemOnChip for Rp2040 {
    type Dma = Rp2040Dma;

    const DMA: Self::Dma = Rp2040Dma;
    const DEFAULT_CPU_HZ: u32 = XOSC_HZ;
    const APB_HZ: u32 = XOSC_HZ;
    const CAPABILITIES: hal::soc::SocCapabilities = hal::soc::SocCapabilities {
        cores: 2,
        interrupt_matrix: false,
        cache_off_execution: false,
        hardware_rng: false,
    };
    // APB peripheral window. The RP2040 maps its peripherals from
    // 0x4000_0000; SIO at 0xD000_0000 is not a manifest peripheral base.
    const PERIPHERAL_WINDOW: (u32, u32) = (0x4000_0000, 0x4007_FFFF);
    const MAX_GPIO: u8 = 29;

    unsafe fn configure_cpu_clock() {
        // Establish a deliberately modest but known clock without depending
        // on a PLL setup inherited from a preceding image. RP2040 datasheet
        // sections 2.16.7 and 2.15.6: start the 12 MHz crystal, wait stable,
        // then glitchlessly select it as clk_ref. clk_sys resets to clk_ref.
        #[cfg(target_arch = "arm")]
        unsafe {
            XOSC_STARTUP.write_volatile(47);
            XOSC_CTRL.write_volatile(0x00fa_baa0);
            assert!(
                wait_for_bits(XOSC_STATUS, 1 << 31),
                "RP2040 XOSC did not stabilize"
            );
            // A soft reset may inherit clk_sys's AUX/PLL selection. Move it
            // to clk_ref first, then make clk_ref the crystal, so the stated
            // 12 MHz result does not depend on reset provenance.
            CLK_SYS_CTRL.write_volatile(0);
            assert!(
                wait_for_bits(CLK_SYS_SELECTED, 1),
                "RP2040 clk_sys did not select clk_ref"
            );
            CLK_REF_CTRL.write_volatile(2);
            assert!(
                wait_for_bits(CLK_REF_SELECTED, 1 << 2),
                "RP2040 clk_ref did not select XOSC"
            );
        }
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

    #[test]
    fn first_kernel_clock_is_the_wio_crystal_frequency() {
        use hal::soc::SystemOnChip;
        assert_eq!(Rp2040::DEFAULT_CPU_HZ, XOSC_HZ);
        assert_eq!(Rp2040::APB_HZ, XOSC_HZ);
    }
}

/// The RP2040 has no low-power sleep FSM the kernel drives today, so it takes
/// the [`hal::power::LowPower`] defaults: every call reports `Unsupported`.
impl hal::power::LowPower for Rp2040 {}
