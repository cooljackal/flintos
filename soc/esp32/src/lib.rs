// SPDX-License-Identifier: Apache-2.0

//! ESP32 SoC support.
//!
//! This crate owns what is true of the *chip* rather than of the board or of
//! the CPU core:
//!
//! - [`addr`] — peripheral base addresses and interrupt source numbers
//! - [`ctrl`] — the I2C/SPI/UART controllers as enums, each attribute a
//!   `const fn` of the variant, plus the `*Port` structs that pair one with
//!   its pin configuration
//! - [`dport`] — peripheral clock gating and reset
//! - [`io_mux`] — the pad configuration registers and their non-linear
//!   offset table
//! - [`gpio_matrix`] — signal routing, and the signal index map
//! - [`pinmux`] — [`Esp32PinMux`], the chip's implementation of
//!   [`hal::PinMux`]
//!
//! What is deliberately *not* here: peripherals. RMT, the watchdogs and the
//! RNG were modules of this crate and are now `drivers/physical/esp32/*`,
//! because a peripheral is something you write a driver for, while this crate
//! holds what every one of those drivers needs underneath it. The test is
//! whether a second peripheral would want it: an address map and a pin router
//! yes, a pulse generator no.
//!
//! It sits between the arch layer (Xtensa LX6: traps, context switch, tick,
//! which the ESP32 shares with other Xtensa parts) and the board layer (which
//! pin is wired to what, which differs between two boards carrying the same
//! chip). Before this split existed, board manifests carried peripheral base
//! addresses and every physical driver kept its own private copy of the
//! IO_MUX offset table.
//!
//! Everything here is specific to the *classic* ESP32 (and the PICO-D4 SiP
//! built on it). The S2, S3 and C3 have different peripheral maps and, in the
//! C3's case, a different core entirely; they get their own crates.

#![no_std]
// Xtensa inline asm is unstable, and `soc-esp32` is one of the crates that
// builds *both* ways: for the chip on the Espressif nightly, and for the host
// on stable so its unit tests run in ordinary CI. An unconditional
// `feature(asm_experimental_arch)` would be E0554 on the stable host build and
// take those tests with it.
//
// `arch-xtensa` declares the feature unconditionally and gets away with it
// because the kernel scopes that crate to `target_os = "none"`. This one has
// no such luxury.
#![cfg_attr(target_arch = "xtensa", feature(asm_experimental_arch))]

/// The ESP-IDF application image header. A chip-boot artifact, not a CPU
/// one -- the second-stage bootloader reads it, and the Xtensa core knows
/// nothing about it.
pub mod app_desc;
pub mod addr;
pub mod appcpu;
pub mod cpu_clk;
pub mod crosscore;
pub mod ctrl;
pub mod dma;
pub mod efuse;
pub mod dport;
pub mod gpio_matrix;
pub mod intr_map;
pub mod io_mux;
pub mod pinmux;
pub mod poll;
pub mod reg;
pub mod reset;
pub mod rtc;
pub mod sar;
pub mod sleep;

pub use ctrl::{I2cCtrl, I2cPort, SpiCtrl, SpiPort, UartCtrl, UartPort};
pub use pinmux::Esp32PinMux;

/// The chip's IO_MUX-capable GPIO count. Pins 0-39 exist; 34-39 are
/// input-only, and 20, 24, 28-31 are not bonded out at all.
pub const MAX_GPIO: u8 = 39;

/// APB clock, which every peripheral in this crate is timed against.
///
/// Fixed at 80 MHz regardless of the CPU frequency: the ESP32's CPU clock can
/// be 80, 160 or 240 MHz, but APB stays at 80 unless the chip is put into a
/// low-power mode this kernel does not use. Baud-rate and I2C divisors derive
/// from this, not from the measured CPU frequency.
pub const APB_HZ: u32 = 80_000_000;

/// Classic ESP32 implementation of the kernel's selected-SoC contract.
pub struct Esp32;

impl hal::soc::SystemOnChip for Esp32 {
    type Dma = dma::DmaReach;

    const DMA: Self::Dma = dma::DmaReach;
    const DEFAULT_CPU_HZ: u32 = rtc::DEFAULT_CPU_HZ;
    const APB_HZ: u32 = APB_HZ;
    const CAPABILITIES: hal::soc::SocCapabilities = hal::soc::SocCapabilities {
        cores: 2,
        interrupt_matrix: true,
        cache_off_execution: true,
        hardware_rng: true,
    };
    // DPORT peripheral window (0x3FF4_0000..0x3FF8_0000), widened slightly at
    // both ends so a new base address does not need this updated while a base
    // copied from an unrelated address space is still caught.
    const PERIPHERAL_WINDOW: (u32, u32) = (0x3FF0_0000, 0x3FF8_FFFF);
    const MAX_GPIO: u8 = MAX_GPIO;

    unsafe fn configure_cpu_clock() {
        #[cfg(target_arch = "xtensa")]
        unsafe {
            cpu_clk::set_240mhz();
        }
    }

    unsafe fn reset_cause() -> u32 {
        #[cfg(target_arch = "xtensa")]
        {
            unsafe { reset::cause() }
        }
        #[cfg(not(target_arch = "xtensa"))]
        {
            0
        }
    }

    fn reset_cause_name(cause: u32) -> &'static str {
        reset::name(cause)
    }

    fn measure_cpu_hz(cycle_count: fn() -> Option<u32>) -> Option<u32> {
        #[cfg(target_arch = "xtensa")]
        {
            const MEASURE_RTC_TICKS: u64 = 1500;
            const MEASURE_TIMEOUT_CYCLES: u32 = 50_000_000;
            const RTC_POLLS: u32 = 10_000;

            (|| unsafe {
                let rtc0 = rtc::counter(RTC_POLLS)?;
                let c0 = cycle_count()?;
                loop {
                    let elapsed_rtc = rtc::counter(RTC_POLLS)?.wrapping_sub(rtc0);
                    if elapsed_rtc >= MEASURE_RTC_TICKS {
                        let cycles = cycle_count()?.wrapping_sub(c0) as u64;
                        return rtc::round_to_plausible(
                            cycles * rtc::SLOW_HZ_NOMINAL / elapsed_rtc,
                        );
                    }
                    if cycle_count()?.wrapping_sub(c0) > MEASURE_TIMEOUT_CYCLES {
                        return None;
                    }
                }
            })()
        }
        #[cfg(not(target_arch = "xtensa"))]
        {
            let _ = cycle_count;
            None
        }
    }
}

/// Serialises tests that share this crate's global hardware bookkeeping.
///
/// The DMA channel table is one set of state for the whole process, so the
/// default thread-per-test would have several tests claiming channels at once
/// and blaming each other's failures.
#[cfg(test)]
extern crate std;

#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod soc_contract_tests {
    use super::*;
    use hal::dma::DmaReach as _;
    use hal::soc::SystemOnChip as _;

    const _: () = assert!(Esp32::APB_HZ == APB_HZ);
    const _: () = assert!(Esp32::DEFAULT_CPU_HZ == rtc::DEFAULT_CPU_HZ);
    const _: () = assert!(Esp32::CAPABILITIES.cores == 2);
    const _: () = assert!(Esp32::CAPABILITIES.interrupt_matrix);
    const _: () = assert!(Esp32::CAPABILITIES.cache_off_execution);
    const _: () = assert!(Esp32::CAPABILITIES.hardware_rng);

    #[test]
    fn selected_soc_dma_rejects_a_wrapped_range() {
        assert!(!Esp32::DMA.reachable(u32::MAX - 1, 4));
    }

    #[test]
    #[cfg(not(target_arch = "xtensa"))]
    fn host_clock_measurement_reports_unmeasured() {
        assert_eq!(Esp32::measure_cpu_hz(|| Some(1)), None);
    }

    #[test]
    fn reset_names_still_come_from_the_existing_decoder() {
        assert_eq!(Esp32::reset_cause_name(1), "power-on");
        assert_eq!(Esp32::reset_cause_name(9), "RTC watchdog (system)");
    }
}
