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

// A PIO owner can retire without waiting on the shared claim lock. The next
// claim transaction reaps these releases before testing collisions. Only the
// owner of the corresponding claimed block may publish its retirement.
#[cfg(any(target_arch = "arm", test))]
static PIO_RETIRED: [core::sync::atomic::AtomicU32; 2] =
    [const { core::sync::atomic::AtomicU32::new(0) }; 2];

#[cfg(any(target_arch = "arm", test))]
fn reap_pio(uart: &mut u8, gpio: &mut u32) {
    use core::sync::atomic::Ordering::{Acquire, Release};
    for (block, entry) in PIO_RETIRED.iter().enumerate() {
        let retired = entry.load(Acquire);
        if retired != 0 {
            *gpio &= !(retired & 0x3fff_ffff);
            *uart &= !(1 << (2 + block));
            // No second publisher for this block can exist until this claim
            // transaction finishes: its old owner still holds the claim bit.
            entry.store(0, Release);
        }
    }
}

#[cfg(target_arch = "arm")]
fn with_claims<T>(f: impl FnOnce(&mut u8, &mut u32) -> T) -> T {
    const CLAIM_LOCK: *mut u32 = (crate::SIO_BASE + 0x100 + 31 * 4) as *mut u32;
    unsafe {
        while CLAIM_LOCK.read_volatile() == 0 {
            core::hint::spin_loop();
        }
        core::arch::asm!("dmb", options(nostack));
        let uart = core::ptr::addr_of_mut!(UART_CLAIMS);
        let gpio = core::ptr::addr_of_mut!(GPIO_CLAIMS);
        let mut uart_value = uart.read_volatile();
        let mut gpio_value = gpio.read_volatile();
        reap_pio(&mut uart_value, &mut gpio_value);
        let result = f(&mut uart_value, &mut gpio_value);
        uart.write_volatile(uart_value);
        gpio.write_volatile(gpio_value);
        core::arch::asm!("dmb", options(nostack));
        CLAIM_LOCK.write_volatile(1);
        result
    }
}

/// An exclusive PIO block and pin bundle. Claims bits 2/3, beside UART 0/1
/// and USB 7. No public constructor, Clone or forced release is available.
pub struct PioLease { block: u8, pins: u32 }

/// Single-attempt acquisition: a busy claim lock is Busy, never an ISR spin.
pub fn try_claim_pio(block: u8, pins: u32) -> Option<PioLease> {
    if block >= 2 || pins & !0x3fff_ffff != 0 { return None; }
    let mask = 1 << (2 + block);
    #[cfg(target_arch = "arm")]
    unsafe {
        const LOCK: *mut u32 = (crate::SIO_BASE + 0x100 + 31 * 4) as *mut u32;
        // Legacy GPIO/UART claim callers can spin on this shared lock from an
        // interrupt. Do not let one preempt this short, single-attempt owner.
        let primask: u32;
        core::arch::asm!("mrs {saved}, PRIMASK", "cpsid i", saved = out(reg) primask, options(nostack));
        if LOCK.read_volatile() == 0 {
            core::arch::asm!("msr PRIMASK, {saved}", saved = in(reg) primask, options(nostack));
            return None;
        }
        core::arch::asm!("dmb", options(nostack));
        let mut peripherals = core::ptr::addr_of!(UART_CLAIMS).read_volatile();
        let mut gpio = core::ptr::addr_of!(GPIO_CLAIMS).read_volatile();
        reap_pio(&mut peripherals, &mut gpio);
        let free = peripherals & mask == 0 && gpio & pins == 0;
        if free { peripherals |= mask; gpio |= pins; }
        core::ptr::addr_of_mut!(UART_CLAIMS).write_volatile(peripherals);
        core::ptr::addr_of_mut!(GPIO_CLAIMS).write_volatile(gpio);
        core::arch::asm!("dmb", options(nostack));
        LOCK.write_volatile(1);
        core::arch::asm!("msr PRIMASK, {saved}", saved = in(reg) primask, options(nostack));
        if !free { return None; }
    }
    #[cfg(not(target_arch = "arm"))]
    {
        use core::sync::atomic::Ordering::{AcqRel, Acquire, Release};
        if UART_CLAIMS.fetch_or(mask, AcqRel) & mask != 0 { return None; }
        let previous = GPIO_CLAIMS.load(Acquire);
        if previous & pins != 0 || GPIO_CLAIMS.compare_exchange(previous, previous | pins, AcqRel, Acquire).is_err() {
            UART_CLAIMS.fetch_and(!mask, Release);
            return None;
        }
    }
    Some(PioLease { block, pins })
}

impl Drop for PioLease {
    fn drop(&mut self) {
        #[cfg(target_arch = "arm")]
        unsafe {
            // Hardware must be quiesced by the driver before dropping this
            // token. Publish only after its pin/register writes are complete.
            core::arch::asm!("dmb", options(nostack));
            PIO_RETIRED[usize::from(self.block)].store(
                self.pins | (1 << 31), core::sync::atomic::Ordering::Release,
            );
            core::arch::asm!("dmb", options(nostack));
        }
        #[cfg(not(target_arch = "arm"))]
        {
            GPIO_CLAIMS.fetch_and(!self.pins, core::sync::atomic::Ordering::Release);
            UART_CLAIMS.fetch_and(!(1 << (2 + self.block)), core::sync::atomic::Ordering::Release);
        }
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

    #[test]
    fn pio_bundle_claims_do_not_steal_pins_or_blocks_and_drop_releases_them() {
        let first = try_claim_pio(0, 1 << 26).unwrap();
        assert!(try_claim_pio(0, 1 << 27).is_none());
        assert!(try_claim_pio(1, 1 << 26).is_none());
        assert!(!claim_gpio(26));
        assert!(claim_gpio(27)); // failed block claim did not reserve its pins
        assert!(try_claim_pio(1, (1 << 27) | (1 << 28)).is_none());
        assert!(claim_gpio(28)); // all-or-nothing pin collision
        release_gpio(27); release_gpio(28);
        let second = try_claim_pio(1, 1 << 27).unwrap();
        drop(first); assert!(claim_gpio(26)); release_gpio(26);
        drop(second); assert!(try_claim_pio(0, (1 << 26) | (1 << 27)).is_some());
        assert!(try_claim_pio(2, 0).is_none());
        assert!(try_claim_pio(0, 1 << 30).is_none());
        // Keep any successful lease alive until all contenders have tried.
        // A fetch-or followed by an incorrect rollback would admit two owners.
        extern crate std;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let won = std::sync::Arc::new(core::sync::atomic::AtomicU32::new(0));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let barrier = barrier.clone(); let won = won.clone();
                scope.spawn(move || {
                    barrier.wait();
                    let lease = try_claim_pio(0, 1 << 26);
                    if lease.is_some() { won.fetch_add(1, core::sync::atomic::Ordering::SeqCst); }
                    barrier.wait(); drop(lease);
                });
            }
        });
        // An unrelated concurrent GPIO claim can make the single CAS refuse
        // all contenders; the invariant is at most one owner, then reusable.
        assert!(won.load(core::sync::atomic::Ordering::SeqCst) <= 1);
        assert!(try_claim_pio(0, 1 << 26).is_some());
    }

    #[test]
    fn deferred_pio_reaping_preserves_unrelated_claims_and_handles_pinless_blocks() {
        use core::sync::atomic::Ordering::{Acquire, Release};
        PIO_RETIRED[0].store((1 << 31) | (1 << 26), Release);
        PIO_RETIRED[1].store(1 << 31, Release);
        let mut peripherals=0x8f; // two UARTs, two PIOs and USB
        let mut pins=(1 << 26) | (1 << 3);
        reap_pio(&mut peripherals,&mut pins);
        assert_eq!(peripherals,0x83);
        assert_eq!(pins,1 << 3);
        assert!(PIO_RETIRED.iter().all(|entry| entry.load(Acquire)==0));
        reap_pio(&mut peripherals,&mut pins);
        assert_eq!((peripherals,pins),(0x83,1 << 3));
    }
}
