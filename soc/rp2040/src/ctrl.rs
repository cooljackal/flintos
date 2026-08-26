// SPDX-License-Identifier: Apache-2.0

//! Typed RP2040 peripheral identities and cross-core ownership claims.

use hal::bus::{I2cConfig, SpiConfig, UartConfig};

use crate::{
    I2C0_BASE, I2C1_BASE, IRQ_I2C0, IRQ_I2C1, IRQ_SPI0, IRQ_SPI1, IRQ_UART0, IRQ_UART1,
    RESET_I2C0, RESET_I2C1, RESET_SPI0, RESET_SPI1, RESET_UART0, RESET_UART1, SPI0_BASE,
    SPI1_BASE, UART0_BASE, UART1_BASE,
};

#[cfg(target_arch = "arm")]
static mut UART_CLAIMS: u8 = 0;
#[cfg(target_arch = "arm")]
static mut GPIO_CLAIMS: u32 = 0;
#[cfg(not(target_arch = "arm"))]
static UART_CLAIMS: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
#[cfg(not(target_arch = "arm"))]
static GPIO_CLAIMS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

#[cfg(target_arch = "arm")]
fn with_claims<T>(f: impl FnOnce(&mut u8, &mut u32) -> T) -> T {
    const CLAIM_LOCK: *mut u32 = (crate::SIO_BASE + 0x100 + 31 * 4) as *mut u32;
    unsafe {
        while CLAIM_LOCK.read_volatile() == 0 {
            core::hint::spin_loop();
        }
        let uart = core::ptr::addr_of_mut!(UART_CLAIMS);
        let gpio = core::ptr::addr_of_mut!(GPIO_CLAIMS);
        let mut uart_value = uart.read_volatile();
        let mut gpio_value = gpio.read_volatile();
        let result = f(&mut uart_value, &mut gpio_value);
        uart.write_volatile(uart_value);
        gpio.write_volatile(gpio_value);
        CLAIM_LOCK.write_volatile(1);
        result
    }
}

pub fn claim_uart(ctrl: UartCtrl) -> bool {
    let mask = 1 << ctrl.instance();
    #[cfg(target_arch = "arm")]
    {
        return with_claims(|uart, _| {
            let free = *uart & mask == 0;
            *uart |= mask;
            free
        });
    }
    #[cfg(not(target_arch = "arm"))]
    {
        UART_CLAIMS.fetch_or(mask, core::sync::atomic::Ordering::AcqRel) & mask == 0
    }
}

pub fn release_uart(ctrl: UartCtrl) {
    let mask = !(1 << ctrl.instance());
    #[cfg(target_arch = "arm")]
    with_claims(|uart, _| *uart &= mask);
    #[cfg(not(target_arch = "arm"))]
    UART_CLAIMS.fetch_and(mask, core::sync::atomic::Ordering::Release);
}

/// USB uses bit 7 of the peripheral ownership word; UARTs occupy bits 0/1.
pub fn claim_usb() -> bool {
    #[cfg(target_arch = "arm")]
    { with_claims(|claims, _| { let free = *claims & 0x80 == 0; *claims |= 0x80; free }) }
    #[cfg(not(target_arch = "arm"))]
    { UART_CLAIMS.fetch_or(0x80, core::sync::atomic::Ordering::AcqRel) & 0x80 == 0 }
}
pub fn release_usb() {
    #[cfg(target_arch = "arm")]
    with_claims(|claims, _| *claims &= !0x80);
    #[cfg(not(target_arch = "arm"))]
    UART_CLAIMS.fetch_and(!0x80, core::sync::atomic::Ordering::Release);
}

pub fn claim_gpio(pin: u8) -> bool {
    if pin >= 30 {
        return false;
    }
    let mask = 1 << pin;
    #[cfg(target_arch = "arm")]
    {
        return with_claims(|_, gpio| {
            let free = *gpio & mask == 0;
            *gpio |= mask;
            free
        });
    }
    #[cfg(not(target_arch = "arm"))]
    {
        GPIO_CLAIMS.fetch_or(mask, core::sync::atomic::Ordering::AcqRel) & mask == 0
    }
}

pub fn release_gpio(pin: u8) {
    if pin >= 30 {
        return;
    }
    let mask = !(1 << pin);
    #[cfg(target_arch = "arm")]
    with_claims(|_, gpio| *gpio &= mask);
    #[cfg(not(target_arch = "arm"))]
    GPIO_CLAIMS.fetch_and(mask, core::sync::atomic::Ordering::Release);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UartCtrl {
    Uart0,
    Uart1,
}

impl UartCtrl {
    pub const fn base(self) -> u32 {
        match self {
            Self::Uart0 => UART0_BASE,
            Self::Uart1 => UART1_BASE,
        }
    }

    pub const fn irq(self) -> u8 {
        match self {
            Self::Uart0 => IRQ_UART0,
            Self::Uart1 => IRQ_UART1,
        }
    }

    pub const fn reset_mask(self) -> u32 {
        match self {
            Self::Uart0 => RESET_UART0,
            Self::Uart1 => RESET_UART1,
        }
    }

    pub const fn instance(self) -> u8 {
        match self {
            Self::Uart0 => 0,
            Self::Uart1 => 1,
        }
    }

    pub const fn from_base(base: u32) -> Option<Self> {
        match base {
            UART0_BASE => Some(Self::Uart0),
            UART1_BASE => Some(Self::Uart1),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartPort {
    pub ctrl: UartCtrl,
    pub cfg: UartConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioPort {
    pub pin: u8,
}

macro_rules! controller {
    ($name:ident, $port:ident, $zero:ident, $one:ident, $base0:ident, $base1:ident,
     $irq0:ident, $irq1:ident, $reset0:ident, $reset1:ident, $cfg:ty) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $zero,
            $one,
        }

        impl $name {
            pub const fn base(self) -> u32 {
                match self { Self::$zero => $base0, Self::$one => $base1 }
            }
            pub const fn irq(self) -> u8 {
                match self { Self::$zero => $irq0, Self::$one => $irq1 }
            }
            pub const fn reset_mask(self) -> u32 {
                match self { Self::$zero => $reset0, Self::$one => $reset1 }
            }
            pub const fn instance(self) -> u8 {
                match self { Self::$zero => 0, Self::$one => 1 }
            }
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $port {
            pub ctrl: $name,
            pub cfg: $cfg,
        }
    };
}

controller!(
    SpiCtrl, SpiPort, Spi0, Spi1, SPI0_BASE, SPI1_BASE, IRQ_SPI0, IRQ_SPI1,
    RESET_SPI0, RESET_SPI1, SpiConfig
);
controller!(
    I2cCtrl, I2cPort, I2c0, I2c1, I2C0_BASE, I2C1_BASE, IRQ_I2C0, IRQ_I2C1,
    RESET_I2C0, RESET_I2C1, I2cConfig
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uart_facts_round_trip() {
        for ctrl in [UartCtrl::Uart0, UartCtrl::Uart1] {
            assert_eq!(UartCtrl::from_base(ctrl.base()), Some(ctrl));
            assert_eq!(ctrl.instance(), u8::from(ctrl == UartCtrl::Uart1));
            assert_ne!(ctrl.irq(), 0);
            assert_ne!(ctrl.reset_mask(), 0);
        }
        assert_eq!(UartCtrl::from_base(0), None);
    }

    #[test]
    fn claims_are_per_resource() {
        release_uart(UartCtrl::Uart0);
        release_uart(UartCtrl::Uart1);
        release_gpio(13);
        release_gpio(14);
        assert!(claim_uart(UartCtrl::Uart0));
        assert!(!claim_uart(UartCtrl::Uart0));
        assert!(claim_uart(UartCtrl::Uart1));
        assert!(claim_gpio(13));
        assert!(!claim_gpio(13));
        assert!(claim_gpio(14));
        assert!(!claim_gpio(30));
        release_uart(UartCtrl::Uart0);
        release_uart(UartCtrl::Uart1);
        release_gpio(13);
        release_gpio(14);
    }

    #[test]
    fn spi_and_i2c_controller_facts_are_typed() {
        assert_eq!((SpiCtrl::Spi0.base(), SpiCtrl::Spi1.base()), (SPI0_BASE, SPI1_BASE));
        assert_eq!((I2cCtrl::I2c0.irq(), I2cCtrl::I2c1.irq()), (IRQ_I2C0, IRQ_I2C1));
        assert_ne!(SpiCtrl::Spi0.reset_mask(), SpiCtrl::Spi1.reset_mask());
        assert_ne!(I2cCtrl::I2c0.reset_mask(), I2cCtrl::I2c1.reset_mask());
    }
}
