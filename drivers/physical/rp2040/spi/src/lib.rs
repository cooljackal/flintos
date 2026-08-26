// SPDX-License-Identifier: Apache-2.0

//! RP2040's two ARM PL022 SPI controllers.

#![no_std]

use hal::bus::{BusConfig, BusError, BusResult, BusSpeed, PhysicalBus, PhysicalTransfer, SpiMode};
use soc_rp2040::{ctrl, unreset, IO_BANK0_BASE, PADS_BANK0_BASE, RESET_IO_BANK0, RESET_PADS_BANK0};

const CR0: u32 = 0x00;
const CR1: u32 = 0x04;
const DR: u32 = 0x08;
const SR: u32 = 0x0c;
const CPSR: u32 = 0x10;
const ICR: u32 = 0x20;
const CR1_LBM: u32 = 1;
const CR1_SSE: u32 = 1 << 1;
const SR_TNF: u32 = 1 << 1;
const SR_RNE: u32 = 1 << 2;
const SR_BSY: u32 = 1 << 4;
const FIFO_DEPTH: usize = 8;
const TIMEOUT_US: u32 = 50_000;

#[cfg(target_arch = "arm")]
fn now_us() -> u32 {
    soc_rp2040::timer_us()
}
#[cfg(not(target_arch = "arm"))]
fn now_us() -> u32 {
    static CLOCK: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
    CLOCK.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
}

#[cfg(target_arch = "arm")]
static mut CLAIMED: u8 = 0;
#[cfg(not(target_arch = "arm"))]
static CLAIMED: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

fn claim(instance: u8) -> bool {
    let mask = 1 << instance;
    #[cfg(target_arch = "arm")]
    unsafe {
        const LOCK: *mut u32 = (soc_rp2040::SIO_BASE + 0x100 + 31 * 4) as *mut u32;
        while LOCK.read_volatile() == 0 {
            core::hint::spin_loop();
        }
        let claims = core::ptr::addr_of_mut!(CLAIMED);
        let value = claims.read_volatile();
        let free = value & mask == 0;
        claims.write_volatile(value | mask);
        LOCK.write_volatile(1);
        free
    }
    #[cfg(not(target_arch = "arm"))]
    {
        CLAIMED.fetch_or(mask, core::sync::atomic::Ordering::AcqRel) & mask == 0
    }
}

fn release(instance: u8) {
    let mask = !(1 << instance);
    #[cfg(target_arch = "arm")]
    unsafe {
        const LOCK: *mut u32 = (soc_rp2040::SIO_BASE + 0x100 + 31 * 4) as *mut u32;
        while LOCK.read_volatile() == 0 {
            core::hint::spin_loop();
        }
        let claims = core::ptr::addr_of_mut!(CLAIMED);
        claims.write_volatile(claims.read_volatile() & mask);
        LOCK.write_volatile(1);
    }
    #[cfg(not(target_arch = "arm"))]
    CLAIMED.fetch_and(mask, core::sync::atomic::Ordering::Release);
}

fn pin_valid(instance: u8, pin: u8, role: u8) -> bool {
    if pin >= 30 {
        return false;
    }
    let group = pin & 0x0f;
    match instance {
        0 => group == role || group == role + 4,
        1 => group == role + 8 || group == role + 12,
        _ => false,
    }
}

fn divider(peri_hz: u32, requested_hz: u32) -> Option<(u8, u8, u32)> {
    if peri_hz == 0 || requested_hz == 0 {
        return None;
    }
    let mut best = None;
    for prescale in (2..=254u32).step_by(2) {
        // One candidate per prescaler: the smallest serial divider that
        // does not overspeed. Avoid 32,512 software divisions on Cortex-M0+.
        let serial = u64::from(peri_hz)
            .div_ceil(u64::from(prescale) * u64::from(requested_hz))
            .max(1) as u32;
        if serial <= 256 {
            let actual = peri_hz / (prescale * serial);
            if actual <= requested_hz && best.is_none_or(|(_, _, prior)| actual > prior) {
                best = Some((prescale as u8, (serial - 1) as u8, actual));
            }
        }
    }
    best
}

pub struct Rp2040Spi {
    ctrl: ctrl::SpiCtrl,
    pins: [u8; 3],
    cr0: u32,
}

impl Rp2040Spi {
    pub fn open(port: &ctrl::SpiPort) -> hal::Result<Self> {
        let pins = [port.cfg.mosi, port.cfg.miso, port.cfg.sck];
        if pins[0] == pins[1]
            || pins[0] == pins[2]
            || pins[1] == pins[2]
            || !pin_valid(port.ctrl.instance(), pins[0], 3)
            || !pin_valid(port.ctrl.instance(), pins[1], 0)
            || !pin_valid(port.ctrl.instance(), pins[2], 2)
        {
            return Err(BusError::InvalidConfig.into());
        }
        if !claim(port.ctrl.instance()) {
            return Err(BusError::Busy.into());
        }
        for (claimed, &pin) in pins.iter().enumerate() {
            if !ctrl::claim_gpio(pin) {
                for &owned in &pins[..claimed] {
                    ctrl::release_gpio(owned);
                }
                release(port.ctrl.instance());
                return Err(BusError::Busy.into());
            }
        }
        let mut spi = Self {
            ctrl: port.ctrl,
            pins,
            cr0: 7,
        };
        if let Err(error) = spi.init(&BusConfig::Spi(port.cfg)) {
            drop(spi);
            return Err(error.into());
        }
        Ok(spi)
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.ctrl.base() + offset) as *mut u32
    }

    fn apply_speed(&self, speed: BusSpeed) -> BusResult<()> {
        let (prescale, scr, _) =
            divider(soc_rp2040::XOSC_HZ, speed.hz()).ok_or(BusError::InvalidConfig)?;
        unsafe {
            self.reg(CR1)
                .write_volatile(self.reg(CR1).read_volatile() & !CR1_SSE);
            self.reg(CPSR).write_volatile(u32::from(prescale));
            self.reg(CR0)
                .write_volatile(self.cr0 | (u32::from(scr) << 8));
            self.reg(CR1)
                .write_volatile(self.reg(CR1).read_volatile() | CR1_SSE);
        }
        Ok(())
    }

    /// Enable the PL022's documented internal TX-to-RX loopback.
    pub fn set_loopback(&self, enabled: bool) {
        unsafe {
            let cr1 = self.reg(CR1);
            let value = cr1.read_volatile();
            cr1.write_volatile(if enabled {
                value | CR1_LBM
            } else {
                value & !CR1_LBM
            });
        }
    }

    fn recover(&self) {
        unsafe {
            let cr0 = self.reg(CR0).read_volatile();
            let cr1 = self.reg(CR1).read_volatile() | CR1_SSE;
            let prescale = self.reg(CPSR).read_volatile();
            // Disabling SSE alone leaves queued bytes in the FIFOs. Reset
            // both FIFOs so the next exchange cannot consume stale data.
            soc_rp2040::reset(self.ctrl.reset_mask());
            unreset(self.ctrl.reset_mask());
            self.reg(CR0).write_volatile(cr0);
            self.reg(CPSR).write_volatile(prescale);
            self.reg(CR1).write_volatile(cr1);
        }
    }
}

impl PhysicalBus for Rp2040Spi {
    fn init(&mut self, config: &BusConfig) -> BusResult<()> {
        let BusConfig::Spi(config) = config else {
            return Err(BusError::InvalidConfig);
        };
        if [config.mosi, config.miso, config.sck] != self.pins {
            return Err(BusError::InvalidConfig);
        }
        divider(soc_rp2040::XOSC_HZ, config.max_speed.hz()).ok_or(BusError::InvalidConfig)?;
        self.cr0 = 7 | match config.mode {
            SpiMode::Mode0 => 0,
            SpiMode::Mode1 => 1 << 7,
            SpiMode::Mode2 => 1 << 6,
            SpiMode::Mode3 => (1 << 6) | (1 << 7),
        };
        unsafe {
            soc_rp2040::enable_peripheral_clock();
            soc_rp2040::reset(self.ctrl.reset_mask());
            unreset(self.ctrl.reset_mask() | RESET_IO_BANK0 | RESET_PADS_BANK0);
            for &pin in &self.pins {
                let pad = (PADS_BANK0_BASE + 4 + u32::from(pin) * 4) as *mut u32;
                pad.write_volatile(
                    (pad.read_volatile() & !((1 << 7) | (1 << 3) | (1 << 2))) | (1 << 6),
                );
                ((IO_BANK0_BASE + 4 + u32::from(pin) * 8) as *mut u32).write_volatile(1);
            }
            self.reg(CR1).write_volatile(0);
        }
        self.apply_speed(config.max_speed)
    }
}

impl PhysicalTransfer for Rp2040Spi {
    fn exchange(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        let len = tx.len().min(rx.len());
        if len == 0 {
            return Ok(());
        }
        let start = now_us();
        let mut sent = 0usize;
        let mut received = 0usize;
        while received < len {
            let status = unsafe { self.reg(SR).read_volatile() };
            if sent < len && status & SR_TNF != 0 && received + FIFO_DEPTH > sent {
                unsafe { self.reg(DR).write_volatile(u32::from(tx[sent])) };
                sent += 1;
            }
            if status & SR_RNE != 0 {
                rx[received] = unsafe { self.reg(DR).read_volatile() as u8 };
                received += 1;
            }
            if now_us().wrapping_sub(start) >= TIMEOUT_US {
                self.recover();
                return Err(BusError::Timeout);
            }
        }
        while unsafe { self.reg(SR).read_volatile() } & SR_BSY != 0 {
            if now_us().wrapping_sub(start) >= TIMEOUT_US {
                self.recover();
                return Err(BusError::Timeout);
            }
        }
        unsafe { self.reg(ICR).write_volatile(3) };
        Ok(())
    }

    fn set_speed(&self, speed: BusSpeed) -> BusResult<()> {
        self.apply_speed(speed)
    }
}

impl Drop for Rp2040Spi {
    fn drop(&mut self) {
        unsafe { self.reg(CR1).write_volatile(0) };
        for &pin in &self.pins {
            ctrl::release_gpio(pin);
        }
        release(self.ctrl.instance());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_match_the_generated_pico_sdk_header() {
        assert_eq!((CR0, CR1, DR, SR, CPSR, ICR), (0, 4, 8, 12, 16, 32));
        assert_eq!((CR1_LBM, CR1_SSE, SR_TNF, SR_RNE, SR_BSY), (1, 2, 2, 4, 16));
    }

    #[test]
    fn fixed_function_pins_are_not_arbitrary() {
        assert!(pin_valid(0, 19, 3));
        assert!(pin_valid(0, 16, 0));
        assert!(pin_valid(0, 18, 2));
        assert!(!pin_valid(1, 19, 3));
    }

    #[test]
    fn divider_never_exceeds_the_request() {
        assert_eq!(divider(12_000_000, 1_000_000), Some((2, 5, 1_000_000)));
        assert_eq!(divider(12_000_000, 0), None);
        assert!(divider(12_000_000, 100).is_none());
        for requested in [
            185,
            1_001,
            99_999,
            100_000,
            400_000,
            999_999,
            6_000_000,
            u32::MAX,
        ] {
            let (prescale, scr, actual) = divider(12_000_000, requested).unwrap();
            assert_eq!(prescale & 1, 0);
            assert!(prescale >= 2);
            assert_eq!(
                actual,
                12_000_000 / (u32::from(prescale) * (u32::from(scr) + 1))
            );
            assert!(
                12_000_000u64 <= u64::from(requested) * u64::from(prescale) * (u64::from(scr) + 1)
            );
        }
        assert_eq!(divider(0, 1_000_000), None);
    }

    #[test]
    fn controller_claims_are_exclusive_and_reusable() {
        assert!(claim(0));
        assert!(!claim(0));
        assert!(claim(1));
        release(0);
        assert!(claim(0));
        release(0);
        release(1);
    }
}
