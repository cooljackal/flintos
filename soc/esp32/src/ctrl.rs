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
//! have to agree. Each carries the per-bus config type that matches its
//! controller — an [`I2cPort`] holds an [`I2cConfig`], never a SPI one — so a
//! mismatch is a type error rather than a run-time `InvalidConfig`.

use hal::bus::{I2cConfig, SpiConfig, UartConfig};

use crate::addr;
use crate::dport::ClockBit;

/// Define a peripheral-controller enum whose every attribute is a `const fn` of
/// the variant, from one list of variants. `base`/`irq`/`clock`/`instance`
/// become `match`es, and `from_base` is their inverse — so the map and its
/// inverse are written once and cannot drift (the round-trip is still checked in
/// the tests below). A controller with an extra attribute (SPI's `dma_host`)
/// adds it in its own `impl` block.
macro_rules! controller_enum {
    (
        $(#[$emeta:meta])*
        $name:ident {
            $(
                $(#[$vmeta:meta])*
                $variant:ident => {
                    base: $base:expr,
                    irq: $irq:expr,
                    clock: $clock:expr,
                    instance: $inst:expr $(,)?
                }
            ),+ $(,)?
        }
    ) => {
        $(#[$emeta])*
        #[derive(Copy, Clone, PartialEq, Eq, Debug)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )+
        }

        impl $name {
            /// Register block base address.
            pub const fn base(self) -> u32 {
                match self { $( Self::$variant => $base, )+ }
            }

            /// Peripheral interrupt source, for the interrupt matrix.
            pub const fn irq(self) -> u8 {
                match self { $( Self::$variant => $irq, )+ }
            }

            /// DPORT clock-enable / reset bit.
            pub const fn clock(self) -> ClockBit {
                match self { $( Self::$variant => $clock, )+ }
            }

            /// Controller instance number, as the matching `Signal(n)` names it.
            pub const fn instance(self) -> u8 {
                match self { $( Self::$variant => $inst, )+ }
            }

            /// The controller at a base address, if there is one. A base no
            /// variant claims (an unknown address, or SPI1's boot-flash base)
            /// yields `None`.
            pub const fn from_base(base: u32) -> Option<Self> {
                // An `if` chain, not a `match`: a macro `:expr` fragment cannot
                // appear in pattern position, and these bases are `const`s, not
                // literals.
                $( if base == $base { return Some(Self::$variant); } )+
                None
            }
        }
    };
}

controller_enum! {
    /// One of the two I2C controllers.
    I2cCtrl {
        I2c0 => { base: addr::I2C0_BASE, irq: addr::IRQ_I2C0, clock: ClockBit::I2C0, instance: 0 },
        I2c1 => { base: addr::I2C1_BASE, irq: addr::IRQ_I2C1, clock: ClockBit::I2C1, instance: 1 },
    }
}

controller_enum! {
    /// One of the two general-purpose SPI controllers.
    ///
    /// SPI1 is absent on purpose: it is wired to the boot flash.
    SpiCtrl {
        /// "HSPI".
        Spi2 => { base: addr::SPI2_BASE, irq: addr::IRQ_SPI2, clock: ClockBit::SPI2, instance: 2 },
        /// "VSPI".
        Spi3 => { base: addr::SPI3_BASE, irq: addr::IRQ_SPI3, clock: ClockBit::SPI3, instance: 3 },
    }
}

impl SpiCtrl {
    /// The DMA crossbar host this controller is served by.
    pub const fn dma_host(self) -> crate::dma::Host {
        match self {
            Self::Spi2 => crate::dma::Host::Spi2,
            Self::Spi3 => crate::dma::Host::Spi3,
        }
    }
}

controller_enum! {
    /// One of the three UART controllers.
    UartCtrl {
        Uart0 => { base: addr::UART0_BASE, irq: addr::IRQ_UART0, clock: ClockBit::UART0, instance: 0 },
        Uart1 => { base: addr::UART1_BASE, irq: addr::IRQ_UART1, clock: ClockBit::UART1, instance: 1 },
        Uart2 => { base: addr::UART2_BASE, irq: addr::IRQ_UART2, clock: ClockBit::UART2, instance: 2 },
    }
}

// ── Ports ───────────────────────────────────────────────────────────────────

/// An I2C controller and the pin configuration it is to be brought up with.
#[derive(Copy, Clone, Debug)]
pub struct I2cPort {
    pub ctrl: I2cCtrl,
    pub cfg: I2cConfig,
}

/// A SPI controller and the pin configuration it is to be brought up with.
#[derive(Copy, Clone, Debug)]
pub struct SpiPort {
    pub ctrl: SpiCtrl,
    pub cfg: SpiConfig,
}

/// A UART controller and the pin configuration it is to be brought up with.
#[derive(Copy, Clone, Debug)]
pub struct UartPort {
    pub ctrl: UartCtrl,
    pub cfg: UartConfig,
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
