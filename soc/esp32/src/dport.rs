// SPDX-License-Identifier: Apache-2.0

//! DPORT: peripheral clock gating and reset.
//!
//! Most ESP32 peripherals come out of reset clock-gated *off* and held in
//! reset. Every register access to such a peripheral reads as zero and writes
//! nowhere, with no fault — so a driver that forgets this looks like a driver
//! with a wrong register map, and behaves identically to one.
//!
//! Bit positions confirmed against esp-idf `soc/dport_reg.h`.

use crate::addr::{DPORT_BASE, LEDC_BASE, RMT_BASE, I2C0_BASE, I2C1_BASE, SPI2_BASE, SPI3_BASE, UART0_BASE, UART1_BASE, UART2_BASE};

const PERIP_CLK_EN: u32 = DPORT_BASE + 0xC0;
const PERIP_RST_EN: u32 = DPORT_BASE + 0xC4;

/// Clock-enable / reset bit for a peripheral, in `DPORT_PERIP_CLK_EN_REG` and
/// `DPORT_PERIP_RST_EN_REG`. The same bit position serves both registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockBit(u32);

impl ClockBit {
    pub const UART0: Self = Self(1 << 2);
    pub const UART1: Self = Self(1 << 5);
    pub const UART2: Self = Self(1 << 23);
    pub const SPI2: Self = Self(1 << 6);
    pub const SPI3: Self = Self(1 << 16);
    pub const I2C0: Self = Self(1 << 7);
    pub const I2C1: Self = Self(1 << 18);
    pub const RMT: Self = Self(1 << 9);
    /// `DPORT_LEDC_CLK_EN`.
    pub const LEDC: Self = Self(1 << 11);

    pub const fn mask(self) -> u32 {
        self.0
    }
}

/// The clock/reset bit for a peripheral base address.
pub fn clock_bit(base: u32) -> Option<ClockBit> {
    Some(match base {
        UART0_BASE => ClockBit::UART0,
        UART1_BASE => ClockBit::UART1,
        UART2_BASE => ClockBit::UART2,
        SPI2_BASE => ClockBit::SPI2,
        SPI3_BASE => ClockBit::SPI3,
        I2C0_BASE => ClockBit::I2C0,
        I2C1_BASE => ClockBit::I2C1,
        RMT_BASE => ClockBit::RMT,
    LEDC_BASE => ClockBit::LEDC,
        _ => return None,
    })
}

/// Enable a peripheral's clock and release it from reset.
///
/// Call this before touching any of the peripheral's registers.
///
/// # Safety
/// Read-modify-writes two shared DPORT registers. Concurrent callers on
/// different peripherals race on the read-modify-write and can undo each
/// other's bit; call this during single-threaded init, before the scheduler
/// starts, or under a critical section.
pub unsafe fn enable(bit: ClockBit) {
    let clk = PERIP_CLK_EN as *mut u32;
    clk.write_volatile(clk.read_volatile() | bit.mask());
    let rst = PERIP_RST_EN as *mut u32;
    rst.write_volatile(rst.read_volatile() & !bit.mask());
}

/// Gate a peripheral's clock off and hold it in reset.
///
/// # Safety
/// Same read-modify-write race as [`enable`].
pub unsafe fn disable(bit: ClockBit) {
    let rst = PERIP_RST_EN as *mut u32;
    rst.write_volatile(rst.read_volatile() | bit.mask());
    let clk = PERIP_CLK_EN as *mut u32;
    clk.write_volatile(clk.read_volatile() & !bit.mask());
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_addresses_match_the_idf_map() {
        assert_eq!(PERIP_CLK_EN, 0x3FF0_00C0);
        assert_eq!(PERIP_RST_EN, 0x3FF0_00C4);
    }

    #[test]
    fn every_peripheral_has_a_distinct_bit() {
        let bits = [
            ClockBit::UART0,
            ClockBit::UART1,
            ClockBit::UART2,
            ClockBit::SPI2,
            ClockBit::SPI3,
            ClockBit::I2C0,
            ClockBit::I2C1,
        ];
        for (i, a) in bits.iter().enumerate() {
            for b in &bits[i + 1..] {
                assert_ne!(a.mask(), b.mask(), "two peripherals share a clock bit");
            }
        }
    }

    #[test]
    fn base_addresses_map_to_the_right_bits() {
        assert_eq!(clock_bit(I2C0_BASE), Some(ClockBit::I2C0));
        assert_eq!(clock_bit(I2C1_BASE), Some(ClockBit::I2C1));
        assert_eq!(clock_bit(UART0_BASE), Some(ClockBit::UART0));
        assert_eq!(clock_bit(SPI3_BASE), Some(ClockBit::SPI3));
        assert_eq!(clock_bit(RMT_BASE), Some(ClockBit::RMT));
        assert_eq!(clock_bit(LEDC_BASE), Some(ClockBit::LEDC));
        // An address with no clock bit must map to None. This caught a real
        // bug: a base constant that is not imported becomes a *binding* in a
        // match arm rather than a comparison, so it matches everything and
        // every peripheral gets the last arm's clock bit.
        assert_eq!(clock_bit(0xDEAD_BEEF), None);
    }
}
