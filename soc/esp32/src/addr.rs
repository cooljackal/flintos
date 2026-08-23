// SPDX-License-Identifier: Apache-2.0

//! ESP32 peripheral base addresses and interrupt sources.
//!
//! These belong to the chip, not to a board. Two boards carrying the same
//! ESP32 have the same UART0 at the same address; what differs is which pins it
//! comes out on. Board manifests used to spell these out as hex literals, which
//! meant a typo in one board file was invisible from every other.
//!
//! Confirmed against esp-idf `soc/soc.h` and `soc/soc_caps.h`.

use crate::ctrl::{I2cCtrl, SpiCtrl, UartCtrl};

// ── Peripheral register blocks ──────────────────────────────────────────────

pub const UART0_BASE: u32 = 0x3FF4_0000;
pub const UART1_BASE: u32 = 0x3FF5_0000;
pub const UART2_BASE: u32 = 0x3FF6_E000;

/// SPI1, the controller wired to the boot flash. Not for general use.
pub const SPI1_BASE: u32 = 0x3FF4_2000;
/// SPI2, exposed as "HSPI".
pub const SPI2_BASE: u32 = 0x3FF6_4000;
/// SPI3, exposed as "VSPI".
pub const SPI3_BASE: u32 = 0x3FF6_5000;

/// I2C0. I2C1 is a wholly separate register block, not `I2C0 + 0x20`.
pub const I2C0_BASE: u32 = 0x3FF5_3000;
pub const I2C1_BASE: u32 = 0x3FF6_7000;

pub const RMT_BASE: u32 = 0x3FF5_6000;
/// `DR_REG_PCNT_BASE`. Pulse counter: eight units with glitch filtering.
pub const PCNT_BASE: u32 = 0x3FF5_7000;
/// `DR_REG_LEDC_BASE`. PWM: eight high-speed channels over four timers.
pub const LEDC_BASE: u32 = 0x3FF5_9000;

pub const GPIO_BASE: u32 = 0x3FF4_4000;
pub const IO_MUX_BASE: u32 = 0x3FF4_9000;
pub const DPORT_BASE: u32 = 0x3FF0_0000;
pub const RTC_CNTL_BASE: u32 = 0x3FF4_8000;

/// eFuse controller. Block 0 holds the factory MAC address, among much else.
pub const EFUSE_BASE: u32 = 0x3FF5_A000;
pub const TIMG0_BASE: u32 = 0x3FF5_F000;
pub const TIMG1_BASE: u32 = 0x3FF6_0000;

/// `DR_REG_AES_BASE`. The AES accelerator. Lives inside the DPORT window, so
/// its registers take the erratum-safe DPORT access, not plain volatile.
pub const AES_BASE: u32 = 0x3FF0_1000;
/// `DR_REG_SHA_BASE`. The SHA accelerator. Also inside the DPORT window.
pub const SHA_BASE: u32 = 0x3FF0_3000;

// ── Interrupt sources ───────────────────────────────────────────────────────
//
// These are *peripheral interrupt source* numbers fed to the interrupt matrix,
// not CPU interrupt numbers. Names match esp-idf's `ETS_*_INTR_SOURCE`.

pub const IRQ_GPIO: u8 = 22;
pub const IRQ_UART0: u8 = 34;
pub const IRQ_UART1: u8 = 35;
pub const IRQ_UART2: u8 = 36;
pub const IRQ_SPI1: u8 = 29;
pub const IRQ_SPI2: u8 = 30;
pub const IRQ_SPI3: u8 = 31;
pub const IRQ_I2C0: u8 = 49;
pub const IRQ_I2C1: u8 = 50;
/// 47, not 46. Checked against `ETS_RMT_INTR_SOURCE`.
pub const IRQ_RMT: u8 = 47;
/// `ETS_LEDC_INTR_SOURCE`. Fade-complete and timer overflow; unused so far.
pub const IRQ_LEDC: u8 = 45;

/// Timer-group alarms. One source per timer, not one per group — from
/// `ETS_TG0_T0_LEVEL_INTR_SOURCE` onward in esp-idf `soc/periph_defs.h`.
///
/// The `_LEVEL_` sources, not the `_EDGE_` ones: the kernel's crossbar only
/// hands out level-triggered CPU inputs, and `intr_map::route` refuses the
/// rest rather than routing something it could not service.
pub const IRQ_TIMG0_T0: u8 = 14;
pub const IRQ_TIMG0_T1: u8 = 15;
pub const IRQ_TIMG1_T0: u8 = 18;
pub const IRQ_TIMG1_T1: u8 = 19;

// ── Instance lookup ─────────────────────────────────────────────────────────
//
// Drivers are constructed from a base address (that is what the board manifest
// carries), but pin routing is expressed per controller *instance*. These map
// between the two so a driver does not have to keep its own table — the
// mismatch between a driver's idea of "which I2C am I" and the routing layer's
// is exactly the kind of bug that produces a peripheral wired to the wrong
// pins with no diagnostic.
//
// They are thin wrappers over the `ctrl` enums now, which hold the one table;
// a driver built from a `ctrl::SpiCtrl` does not need them at all.

/// Controller instance for a UART base address.
pub const fn uart_instance(base: u32) -> Option<u8> {
    match UartCtrl::from_base(base) {
        Some(c) => Some(c.instance()),
        None => None,
    }
}

/// Controller instance for an I2C base address.
pub const fn i2c_instance(base: u32) -> Option<u8> {
    match I2cCtrl::from_base(base) {
        Some(c) => Some(c.instance()),
        None => None,
    }
}

/// Controller instance for an SPI base address.
///
/// SPI1 is deliberately absent: it drives the boot flash, and handing it to a
/// general-purpose driver bricks the running image.
pub const fn spi_instance(base: u32) -> Option<u8> {
    match SpiCtrl::from_base(base) {
        Some(c) => Some(c.instance()),
        None => None,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i2c_blocks_are_separate_not_adjacent() {
        // A prior revision of the I2C driver documented I2C1 as I2C0 + 0x20.
        assert_ne!(I2C1_BASE - I2C0_BASE, 0x20);
        assert_eq!(I2C0_BASE, 0x3FF5_3000);
        assert_eq!(I2C1_BASE, 0x3FF6_7000);
    }

    #[test]
    fn instance_lookup_round_trips() {
        assert_eq!(uart_instance(UART0_BASE), Some(0));
        assert_eq!(uart_instance(UART2_BASE), Some(2));
        assert_eq!(i2c_instance(I2C0_BASE), Some(0));
        assert_eq!(i2c_instance(I2C1_BASE), Some(1));
        assert_eq!(spi_instance(SPI2_BASE), Some(2));
        assert_eq!(spi_instance(SPI3_BASE), Some(3));
    }

    #[test]
    fn unknown_bases_are_rejected() {
        assert_eq!(uart_instance(0xDEAD_BEEF), None);
        assert_eq!(i2c_instance(0xDEAD_BEEF), None);
        assert_eq!(spi_instance(0xDEAD_BEEF), None);
    }

    #[test]
    fn spi1_is_not_offered_as_a_general_purpose_controller() {
        // It drives the boot flash. Routing it anywhere else bricks the image
        // that is currently executing.
        assert_eq!(spi_instance(SPI1_BASE), None);
    }
}
