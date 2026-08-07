// SPDX-License-Identifier: Apache-2.0

//! ESP32 peripheral base addresses and interrupt sources.
//!
//! These belong to the chip, not to a board. Two boards carrying the same
//! ESP32 have the same UART0 at the same address; what differs is which pins it
//! comes out on. Board manifests used to spell these out as hex literals, which
//! meant a typo in one board file was invisible from every other.
//!
//! Confirmed against esp-idf `soc/soc.h` and `soc/soc_caps.h`.

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

pub const GPIO_BASE: u32 = 0x3FF4_4000;
pub const IO_MUX_BASE: u32 = 0x3FF4_9000;
pub const DPORT_BASE: u32 = 0x3FF0_0000;
pub const RTC_CNTL_BASE: u32 = 0x3FF4_8000;
pub const TIMG0_BASE: u32 = 0x3FF5_F000;
pub const TIMG1_BASE: u32 = 0x3FF6_0000;

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

// ── Instance lookup ─────────────────────────────────────────────────────────
//
// Drivers are constructed from a base address (that is what the board manifest
// carries), but pin routing is expressed per controller *instance*. These map
// between the two so a driver does not have to keep its own table — the
// mismatch between a driver's idea of "which I2C am I" and the routing layer's
// is exactly the kind of bug that produces a peripheral wired to the wrong
// pins with no diagnostic.

/// Controller instance for a UART base address.
pub fn uart_instance(base: u32) -> Option<u8> {
    match base {
        UART0_BASE => Some(0),
        UART1_BASE => Some(1),
        UART2_BASE => Some(2),
        _ => None,
    }
}

/// Controller instance for an I2C base address.
pub fn i2c_instance(base: u32) -> Option<u8> {
    match base {
        I2C0_BASE => Some(0),
        I2C1_BASE => Some(1),
        _ => None,
    }
}

/// Controller instance for an SPI base address.
///
/// SPI1 is deliberately absent: it drives the boot flash, and handing it to a
/// general-purpose driver bricks the running image.
pub fn spi_instance(base: u32) -> Option<u8> {
    match base {
        SPI2_BASE => Some(2),
        SPI3_BASE => Some(3),
        _ => None,
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
