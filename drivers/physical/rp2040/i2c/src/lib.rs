// SPDX-License-Identifier: Apache-2.0

//! RP2040 DesignWare I2C controllers.
//!
//! Polled, 7-bit transactions with a 50 ms total deadline. An address-only
//! zero-length write is unsupported: DesignWare needs a data/read command.
//! Return InvalidConfig rather than silently reading a device during scan().

#![no_std]

use hal::bus::{BusConfig, BusError, BusResult, BusSpeed, PhysicalBus, PhysicalTransfer};
use soc_rp2040::{ctrl, unreset, IO_BANK0_BASE, PADS_BANK0_BASE, RESET_IO_BANK0, RESET_PADS_BANK0};

const CON: u32 = 0x00;
const TAR: u32 = 0x04;
const SAR: u32 = 0x08;
const DATA_CMD: u32 = 0x10;
const FS_SCL_HCNT: u32 = 0x1c;
const FS_SCL_LCNT: u32 = 0x20;
const RAW_INTR_STAT: u32 = 0x34;
const RX_TL: u32 = 0x38;
const TX_TL: u32 = 0x3c;
const CLR_INTR: u32 = 0x40;
const CLR_RD_REQ: u32 = 0x50;
const CLR_TX_ABRT: u32 = 0x54;
const CLR_STOP_DET: u32 = 0x60;
const ENABLE: u32 = 0x6c;
const STATUS: u32 = 0x70;
const SDA_HOLD: u32 = 0x7c;
const TX_ABRT_SOURCE: u32 = 0x80;
const FS_SPKLEN: u32 = 0xa0;

const CON_MASTER: u32 = 1;
const CON_SPEED_FAST: u32 = 2 << 1;
const CON_RESTART: u32 = 1 << 5;
const CON_SLAVE_DISABLE: u32 = 1 << 6;
const CON_STOP_IF_ADDRESSED: u32 = 1 << 7;
const CON_TX_EMPTY_CTRL: u32 = 1 << 8;
const CON_RX_FIFO_HOLD: u32 = 1 << 9;
const CMD_READ: u32 = 1 << 8;
const CMD_STOP: u32 = 1 << 9;
const CMD_RESTART: u32 = 1 << 10;
const RAW_RD_REQ: u32 = 1 << 5;
const RAW_TX_ABRT: u32 = 1 << 6;
const RAW_STOP_DET: u32 = 1 << 9;
const STATUS_TFNF: u32 = 1 << 1;
const STATUS_RFNE: u32 = 1 << 3;
const ABRT_ADDR_NOACK: u32 = 1;
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

fn pins_valid(instance: u8, sda: u8, scl: u8) -> bool {
    sda < 30 && scl < 30 && sda & 1 == 0 && scl == sda + 1 && ((sda >> 1) & 1) == instance
}

fn timing(sys_hz: u32, speed: BusSpeed) -> Option<(u16, u16, u16)> {
    let hz = speed.hz();
    if hz == 0 || hz > 1_000_000 {
        return None;
    }
    let period = (sys_hz + hz / 2) / hz;
    let low = period * 3 / 5;
    let high = period - low;
    if high < 8 || low < 8 || high > u16::MAX.into() || low > u16::MAX.into() {
        return None;
    }
    let hold = if hz < 1_000_000 {
        (sys_hz * 3) / 10_000_000 + 1
    } else {
        (sys_hz * 3) / 25_000_000 + 1
    };
    Some((high as u16, low as u16, hold as u16))
}

pub struct Rp2040I2c {
    ctrl: ctrl::I2cCtrl,
    pins: [u8; 2],
    high: u16,
    low: u16,
    hold: u16,
    slave_address: Option<u8>,
}

impl Rp2040I2c {
    pub fn open(port: &ctrl::I2cPort) -> hal::Result<Self> {
        Self::open_mode(port, None)
    }

    /// Open a controller as a 7-bit slave for a physical target loopback.
    pub fn open_slave(port: &ctrl::I2cPort, address: u8) -> hal::Result<Self> {
        if !(0x08..=0x77).contains(&address) {
            return Err(BusError::InvalidConfig.into());
        }
        Self::open_mode(port, Some(address))
    }

    fn open_mode(port: &ctrl::I2cPort, slave_address: Option<u8>) -> hal::Result<Self> {
        if !pins_valid(port.ctrl.instance(), port.cfg.sda, port.cfg.scl) {
            return Err(BusError::InvalidConfig.into());
        }
        let Some((high, low, hold)) = timing(soc_rp2040::XOSC_HZ, port.cfg.speed) else {
            return Err(BusError::InvalidConfig.into());
        };
        if !claim(port.ctrl.instance()) {
            return Err(BusError::Busy.into());
        }
        let pins = [port.cfg.sda, port.cfg.scl];
        if !ctrl::claim_gpio(pins[0]) {
            release(port.ctrl.instance());
            return Err(BusError::Busy.into());
        }
        if !ctrl::claim_gpio(pins[1]) {
            ctrl::release_gpio(pins[0]);
            release(port.ctrl.instance());
            return Err(BusError::Busy.into());
        }
        let mut i2c = Self {
            ctrl: port.ctrl,
            pins,
            high,
            low,
            hold,
            slave_address,
        };
        if let Err(error) = i2c.init(&BusConfig::I2c(port.cfg)) {
            drop(i2c);
            return Err(error.into());
        }
        Ok(i2c)
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.ctrl.base() + offset) as *mut u32
    }

    unsafe fn program_master(&self) {
        self.reg(ENABLE).write_volatile(0);
        self.reg(CON).write_volatile(
            CON_MASTER | CON_SPEED_FAST | CON_RESTART | CON_SLAVE_DISABLE | CON_TX_EMPTY_CTRL,
        );
        self.reg(FS_SCL_HCNT).write_volatile(u32::from(self.high));
        self.reg(FS_SCL_LCNT).write_volatile(u32::from(self.low));
        self.reg(SDA_HOLD).write_volatile(u32::from(self.hold));
        self.reg(FS_SPKLEN)
            .write_volatile((u32::from(self.low) / 16).max(1));
        self.reg(RX_TL).write_volatile(0);
        self.reg(TX_TL).write_volatile(0);
        self.reg(CLR_INTR).read_volatile();
        self.reg(ENABLE).write_volatile(1);
    }

    fn configure_slave(&self, address: u8) {
        unsafe {
            self.reg(ENABLE).write_volatile(0);
            self.reg(CON)
                .write_volatile(CON_SPEED_FAST | CON_RX_FIFO_HOLD | CON_STOP_IF_ADDRESSED);
            self.reg(SAR).write_volatile(u32::from(address));
            self.reg(RX_TL).write_volatile(0);
            self.reg(CLR_INTR).read_volatile();
            self.reg(ENABLE).write_volatile(1);
        }
    }

    fn recover(&self) {
        unsafe {
            self.reg(ENABLE).write_volatile(0);
            soc_rp2040::reset(self.ctrl.reset_mask());
            unreset(self.ctrl.reset_mask());
            self.program_master();
            if let Some(address) = self.slave_address {
                self.configure_slave(address);
            }
        }
    }

    fn check_abort(&self) -> BusResult<()> {
        let raw = unsafe { self.reg(RAW_INTR_STAT).read_volatile() };
        if raw & RAW_TX_ABRT == 0 {
            return Ok(());
        }
        let source = unsafe { self.reg(TX_ABRT_SOURCE).read_volatile() };
        unsafe {
            self.reg(CLR_TX_ABRT).read_volatile();
        }
        self.recover();
        Err(if source & ABRT_ADDR_NOACK != 0 {
            BusError::DeviceNotResponding
        } else {
            BusError::Busy
        })
    }

    fn wait(&self, bit: u32, set: bool, start: u32) -> BusResult<()> {
        loop {
            self.check_abort()?;
            if now_us().wrapping_sub(start) >= TIMEOUT_US {
                self.recover();
                return Err(BusError::Timeout);
            }
            let present = unsafe { self.reg(STATUS).read_volatile() } & bit != 0;
            if present == set {
                return Ok(());
            }
            core::hint::spin_loop();
        }
    }

    fn transact(&self, address: u8, write: &[u8], read: &mut [u8]) -> BusResult<()> {
        if self.slave_address.is_some()
            || !(0x08..=0x77).contains(&address)
            || (write.is_empty() && read.is_empty())
        {
            return Err(BusError::InvalidConfig);
        }
        let start = now_us();
        unsafe {
            self.reg(ENABLE).write_volatile(0);
            self.reg(TAR).write_volatile(u32::from(address));
            self.reg(CLR_INTR).read_volatile();
            self.reg(ENABLE).write_volatile(1);
        }
        {
            for (index, &byte) in write.iter().enumerate() {
                self.wait(STATUS_TFNF, true, start)?;
                let last = index + 1 == write.len() && read.is_empty();
                unsafe {
                    self.reg(DATA_CMD)
                        .write_volatile(u32::from(byte) | if last { CMD_STOP } else { 0 });
                }
            }
            for index in 0..read.len() {
                self.wait(STATUS_TFNF, true, start)?;
                let command = CMD_READ
                    | if index == 0 && !write.is_empty() {
                        CMD_RESTART
                    } else {
                        0
                    }
                    | if index + 1 == read.len() { CMD_STOP } else { 0 };
                unsafe {
                    self.reg(DATA_CMD).write_volatile(command);
                }
                self.wait(STATUS_RFNE, true, start)?;
                read[index] = unsafe { self.reg(DATA_CMD).read_volatile() as u8 };
            }
        }
        loop {
            self.check_abort()?;
            if unsafe { self.reg(RAW_INTR_STAT).read_volatile() } & RAW_STOP_DET != 0 {
                unsafe {
                    self.reg(CLR_STOP_DET).read_volatile();
                }
                return Ok(());
            }
            if now_us().wrapping_sub(start) >= TIMEOUT_US {
                self.recover();
                return Err(BusError::Timeout);
            }
        }
    }

    /// Serve one master transaction, retaining writes and replying to reads.
    pub fn serve_once(&self, received: &mut [u8], response: &[u8]) -> BusResult<usize> {
        if self.slave_address.is_none() {
            return Err(BusError::InvalidConfig);
        }
        let start = now_us();
        let mut rx_len = 0usize;
        let mut tx_len = 0usize;
        loop {
            let raw = unsafe { self.reg(RAW_INTR_STAT).read_volatile() };
            // STOP can arrive before software drains the final FIFO bytes.
            while unsafe { self.reg(STATUS).read_volatile() } & STATUS_RFNE != 0 {
                let byte = unsafe { self.reg(DATA_CMD).read_volatile() as u8 };
                if rx_len < received.len() {
                    received[rx_len] = byte;
                }
                rx_len += 1;
            }
            if raw & RAW_TX_ABRT != 0 {
                unsafe {
                    self.reg(CLR_TX_ABRT).read_volatile();
                }
            }
            if raw & RAW_RD_REQ != 0 {
                let byte = response.get(tx_len).copied().unwrap_or(0xff);
                unsafe {
                    self.reg(CLR_RD_REQ).read_volatile();
                    self.reg(DATA_CMD).write_volatile(u32::from(byte));
                }
                tx_len += 1;
            }
            if raw & RAW_STOP_DET != 0 {
                unsafe {
                    self.reg(CLR_STOP_DET).read_volatile();
                }
                return if rx_len > received.len() {
                    Err(BusError::InvalidConfig)
                } else {
                    Ok(rx_len)
                };
            }
            if now_us().wrapping_sub(start) >= TIMEOUT_US {
                self.recover();
                return Err(BusError::Timeout);
            }
            core::hint::spin_loop();
        }
    }
}

impl PhysicalBus for Rp2040I2c {
    fn init(&mut self, config: &BusConfig) -> BusResult<()> {
        let BusConfig::I2c(config) = config else {
            return Err(BusError::InvalidConfig);
        };
        if [config.sda, config.scl] != self.pins
            || !pins_valid(self.ctrl.instance(), config.sda, config.scl)
        {
            return Err(BusError::InvalidConfig);
        }
        let Some((high, low, hold)) = timing(soc_rp2040::XOSC_HZ, config.speed) else {
            return Err(BusError::InvalidConfig);
        };
        self.high = high;
        self.low = low;
        self.hold = hold;
        unsafe {
            soc_rp2040::reset(self.ctrl.reset_mask());
            unreset(self.ctrl.reset_mask() | RESET_IO_BANK0 | RESET_PADS_BANK0);
            for &pin in &self.pins {
                let pad = (PADS_BANK0_BASE + 4 + u32::from(pin) * 4) as *mut u32;
                pad.write_volatile(
                    (pad.read_volatile() & !((1 << 7) | (1 << 2))) | (1 << 6) | (1 << 3),
                );
                ((IO_BANK0_BASE + 4 + u32::from(pin) * 8) as *mut u32).write_volatile(3);
            }
            self.program_master();
            if let Some(address) = self.slave_address {
                self.configure_slave(address);
            }
        }
        Ok(())
    }
}

impl PhysicalTransfer for Rp2040I2c {
    fn exchange(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        let (&address, payload) = tx.split_first().ok_or(BusError::InvalidConfig)?;
        self.transact(address, payload, rx)
    }
}

impl Drop for Rp2040I2c {
    fn drop(&mut self) {
        unsafe {
            self.reg(ENABLE).write_volatile(0);
        }
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
        assert_eq!((CON, TAR, SAR, DATA_CMD), (0, 4, 8, 16));
        assert_eq!(
            (ENABLE, STATUS, TX_ABRT_SOURCE, FS_SPKLEN),
            (108, 112, 128, 160)
        );
        assert_eq!((CMD_READ, CMD_STOP, CMD_RESTART), (0x100, 0x200, 0x400));
    }

    #[test]
    fn fixed_function_pin_pairs_select_one_controller() {
        assert!(pins_valid(0, 4, 5));
        assert!(pins_valid(1, 6, 7));
        assert!(!pins_valid(0, 6, 7));
        assert!(!pins_valid(0, 4, 6));
    }

    #[test]
    fn timing_matches_the_pico_sdk_integer_policy() {
        assert_eq!(
            timing(12_000_000, BusSpeed::Standard100k),
            Some((48, 72, 4))
        );
        assert_eq!(timing(12_000_000, BusSpeed::Fast400k), Some((12, 18, 4)));
        assert_eq!(timing(12_000_000, BusSpeed::FastPlus1M), None);
        assert_eq!(timing(12_000_000, BusSpeed::KHz(0)), None);
        assert_eq!(timing(12_000_000, BusSpeed::HighSpeed3M4), None);
    }

    #[test]
    fn failed_pin_claim_does_not_release_someone_elses_pin() {
        let port = ctrl::I2cPort {
            ctrl: ctrl::I2cCtrl::I2c0,
            cfg: hal::bus::I2cConfig {
                sda: 4,
                scl: 5,
                speed: BusSpeed::Standard100k,
            },
        };
        assert!(ctrl::claim_gpio(5));
        // The conflict is detected before any MMIO access on this host.
        assert!(Rp2040I2c::open(&port).is_err());
        assert!(
            !ctrl::claim_gpio(5),
            "must not release the conflicting SCL claim"
        );
        assert!(ctrl::claim_gpio(4), "must roll back its own SDA claim");
        assert!(claim(0), "must roll back its controller claim");
        assert!(!claim(0));
        release(0);
        ctrl::release_gpio(4);
        ctrl::release_gpio(5);
    }

    #[test]
    fn invalid_transactions_are_rejected_before_touching_hardware() {
        let i2c = core::mem::ManuallyDrop::new(Rp2040I2c {
            ctrl: ctrl::I2cCtrl::I2c0,
            pins: [4, 5],
            high: 48,
            low: 72,
            hold: 4,
            slave_address: None,
        });
        assert_eq!(i2c.exchange(&[], &mut []), Err(BusError::InvalidConfig));
        assert_eq!(i2c.exchange(&[0x42], &mut []), Err(BusError::InvalidConfig));
        for address in [0x00, 0x07, 0x78, 0xff] {
            assert_eq!(
                i2c.exchange(&[address, 1], &mut []),
                Err(BusError::InvalidConfig)
            );
        }
    }
}
