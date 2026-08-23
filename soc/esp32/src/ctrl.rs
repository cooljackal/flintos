// SPDX-License-Identifier: Apache-2.0

//! Peripheral controllers as types, and the ports that bind one to a pin
//! configuration.
//!
//! A driver used to be constructed from a bare `u32` base address and then
//! re-derive everything else about its controller at run time: its instance
//! number through [`addr::spi_instance`](crate::addr::spi_instance) on every
//! transfer, its clock bit through [`dport::clock_bit`](crate::dport::clock_bit)
//! at init, its interrupt source from a board-manifest field that could
//! disagree with the base. [`I2cCtrl`], [`SpiCtrl`] and [`UartCtrl`] close
//! the enum once: every attribute is a `const fn` of the variant, evaluated
//! from the same `addr` and `dport` constants the rest of the crate uses, and
//! an invalid combination cannot be spelled.
//!
//! SPI1 is deliberately not a [`SpiCtrl`] variant. It drives the boot flash
//! and [`addr::spi_instance`](crate::addr::spi_instance) already refuses it;
//! the DMA crossbar's [`dma::Host`](crate::dma::Host) keeps a `Spi1` variant
//! only because the hardware has a selector field for it.
//!
//! The port structs pair a controller with its pin configuration so an app
//! passes one `Copy` value instead of a base, an instance and a config that
//! have to agree. They carry the hal [`BusConfig`] today; #110 splits that
//! into per-bus config types and these fields follow.

use hal::bus::BusConfig;

use crate::addr;
use crate::dport::ClockBit;

/// One of the two I2C controllers.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum I2cCtrl {
    I2c0,
    I2c1,
}

impl I2cCtrl {
    /// Register block base address.
    pub const fn base(self) -> u32 {
        match self {
            Self::I2c0 => addr::I2C0_BASE,
            Self::I2c1 => addr::I2C1_BASE,
        }
    }

    /// Peripheral interrupt source, for the interrupt matrix.
    pub const fn irq(self) -> u8 {
        match self {
            Self::I2c0 => addr::IRQ_I2C0,
            Self::I2c1 => addr::IRQ_I2C1,
        }
    }

    /// DPORT clock-enable / reset bit.
    pub const fn clock(self) -> ClockBit {
        match self {
            Self::I2c0 => ClockBit::I2C0,
            Self::I2c1 => ClockBit::I2C1,
        }
    }

    /// Controller instance number, as `Signal::I2cSda(n)` names it.
    pub const fn instance(self) -> u8 {
        match self {
            Self::I2c0 => 0,
            Self::I2c1 => 1,
        }
    }

    /// The controller at a base address, if there is one.
    pub const fn from_base(base: u32) -> Option<Self> {
        match base {
            addr::I2C0_BASE => Some(Self::I2c0),
            addr::I2C1_BASE => Some(Self::I2c1),
            _ => None,
        }
    }
}

/// One of the two general-purpose SPI controllers.
///
/// SPI1 is absent on purpose: it is wired to the boot flash.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SpiCtrl {
    /// "HSPI".
    Spi2,
    /// "VSPI".
    Spi3,
}

impl SpiCtrl {
    /// Register block base address.
    pub const fn base(self) -> u32 {
        match self {
            Self::Spi2 => addr::SPI2_BASE,
            Self::Spi3 => addr::SPI3_BASE,
        }
    }

    /// Peripheral interrupt source, for the interrupt matrix.
    pub const fn irq(self) -> u8 {
        match self {
            Self::Spi2 => addr::IRQ_SPI2,
            Self::Spi3 => addr::IRQ_SPI3,
        }
    }

    /// DPORT clock-enable / reset bit.
    pub const fn clock(self) -> ClockBit {
        match self {
            Self::Spi2 => ClockBit::SPI2,
            Self::Spi3 => ClockBit::SPI3,
        }
    }

    /// Controller instance number, as `Signal::SpiMosi(n)` names it.
    pub const fn instance(self) -> u8 {
        match self {
            Self::Spi2 => 2,
            Self::Spi3 => 3,
        }
    }

    /// The DMA crossbar host this controller is served by.
    pub const fn dma_host(self) -> crate::dma::Host {
        match self {
            Self::Spi2 => crate::dma::Host::Spi2,
            Self::Spi3 => crate::dma::Host::Spi3,
        }
    }

    /// The controller at a base address, if there is one. SPI1's base yields
    /// `None`, as [`addr::spi_instance`] does.
    pub const fn from_base(base: u32) -> Option<Self> {
        match base {
            addr::SPI2_BASE => Some(Self::Spi2),
            addr::SPI3_BASE => Some(Self::Spi3),
            _ => None,
        }
    }
}

/// One of the three UART controllers.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum UartCtrl {
    Uart0,
    Uart1,
    Uart2,
}

impl UartCtrl {
    /// Register block base address.
    pub const fn base(self) -> u32 {
        match self {
            Self::Uart0 => addr::UART0_BASE,
            Self::Uart1 => addr::UART1_BASE,
            Self::Uart2 => addr::UART2_BASE,
        }
    }

    /// Peripheral interrupt source, for the interrupt matrix.
    pub const fn irq(self) -> u8 {
        match self {
            Self::Uart0 => addr::IRQ_UART0,
            Self::Uart1 => addr::IRQ_UART1,
            Self::Uart2 => addr::IRQ_UART2,
        }
    }

    /// DPORT clock-enable / reset bit.
    pub const fn clock(self) -> ClockBit {
        match self {
            Self::Uart0 => ClockBit::UART0,
            Self::Uart1 => ClockBit::UART1,
            Self::Uart2 => ClockBit::UART2,
        }
    }

    /// Controller instance number, as `Signal::UartTx(n)` names it.
    pub const fn instance(self) -> u8 {
        match self {
            Self::Uart0 => 0,
            Self::Uart1 => 1,
            Self::Uart2 => 2,
        }
    }

    /// The controller at a base address, if there is one.
    pub const fn from_base(base: u32) -> Option<Self> {
        match base {
            addr::UART0_BASE => Some(Self::Uart0),
            addr::UART1_BASE => Some(Self::Uart1),
            addr::UART2_BASE => Some(Self::Uart2),
            _ => None,
        }
    }
}

// ── Ports ───────────────────────────────────────────────────────────────────
//
// TODO(#110): `cfg` holds the whole hal `BusConfig` enum, so an `I2cPort`
// can currently carry a `BusConfig::Spi`. Once the config split lands these
// become `I2cConfig` / `SpiConfig` / `UartConfig` and the mismatch is a type
// error.

/// An I2C controller and the pin configuration it is to be brought up with.
#[derive(Copy, Clone, Debug)]
pub struct I2cPort {
    pub ctrl: I2cCtrl,
    pub cfg: BusConfig,
}

/// A SPI controller and the pin configuration it is to be brought up with.
#[derive(Copy, Clone, Debug)]
pub struct SpiPort {
    pub ctrl: SpiCtrl,
    pub cfg: BusConfig,
}

/// A UART controller and the pin configuration it is to be brought up with.
#[derive(Copy, Clone, Debug)]
pub struct UartPort {
    pub ctrl: UartCtrl,
    pub cfg: BusConfig,
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dport;

    const I2C: [I2cCtrl; 2] = [I2cCtrl::I2c0, I2cCtrl::I2c1];
    const SPI: [SpiCtrl; 2] = [SpiCtrl::Spi2, SpiCtrl::Spi3];
    const UART: [UartCtrl; 3] = [UartCtrl::Uart0, UartCtrl::Uart1, UartCtrl::Uart2];

    #[test]
    fn base_round_trips_through_from_base() {
        for c in I2C {
            assert_eq!(I2cCtrl::from_base(c.base()), Some(c));
        }
        for c in SPI {
            assert_eq!(SpiCtrl::from_base(c.base()), Some(c));
        }
        for c in UART {
            assert_eq!(UartCtrl::from_base(c.base()), Some(c));
        }
    }

    #[test]
    fn instance_agrees_with_the_addr_lookups() {
        for c in I2C {
            assert_eq!(addr::i2c_instance(c.base()), Some(c.instance()));
        }
        for c in SPI {
            assert_eq!(addr::spi_instance(c.base()), Some(c.instance()));
        }
        for c in UART {
            assert_eq!(addr::uart_instance(c.base()), Some(c.instance()));
        }
    }

    #[test]
    fn clock_agrees_with_the_dport_lookup() {
        for c in I2C {
            assert_eq!(dport::clock_bit(c.base()), Some(c.clock()));
        }
        for c in SPI {
            assert_eq!(dport::clock_bit(c.base()), Some(c.clock()));
        }
        for c in UART {
            assert_eq!(dport::clock_bit(c.base()), Some(c.clock()));
        }
    }

    #[test]
    fn irqs_are_the_addr_sources() {
        assert_eq!(I2cCtrl::I2c1.irq(), addr::IRQ_I2C1);
        assert_eq!(SpiCtrl::Spi3.irq(), addr::IRQ_SPI3);
        assert_eq!(UartCtrl::Uart2.irq(), addr::IRQ_UART2);
    }

    #[test]
    fn unknown_and_flash_bases_are_rejected() {
        assert_eq!(I2cCtrl::from_base(0xDEAD_BEEF), None);
        assert_eq!(UartCtrl::from_base(0xDEAD_BEEF), None);
        assert_eq!(SpiCtrl::from_base(0xDEAD_BEEF), None);
        assert_eq!(SpiCtrl::from_base(addr::SPI1_BASE), None);
    }

    #[test]
    fn spi_dma_host_matches_the_controller() {
        assert_eq!(SpiCtrl::Spi2.dma_host(), crate::dma::Host::Spi2);
        assert_eq!(SpiCtrl::Spi3.dma_host(), crate::dma::Host::Spi3);
    }

    #[test]
    fn attributes_are_usable_in_const_context() {
        const BASE: u32 = SpiCtrl::Spi2.base();
        const CLK: u32 = SpiCtrl::Spi2.clock().mask();
        const C: Option<SpiCtrl> = SpiCtrl::from_base(BASE);
        assert_eq!(BASE, addr::SPI2_BASE);
        assert_eq!(CLK, ClockBit::SPI2.mask());
        assert_eq!(C, Some(SpiCtrl::Spi2));
    }
}
