// SPDX-License-Identifier: Apache-2.0

//! RP2040's twelve uniform DMA channels.
//!
//! Channel ownership and register programming live with the SoC. Physical
//! drivers choose a peripheral DREQ and hand this module broker-validated SRAM
//! addresses; they never name DMA registers. The abort sequence follows Pico
//! SDK's `dma_channel_cleanup`: disable completion delivery first, abort, then
//! clear the RP2040-E13 late completion before another owner can claim the
//! channel.

use hal::dma::range_within;

#[cfg(any(target_arch = "arm", test))]
use crate::DMA_BASE;
#[cfg(target_arch = "arm")]
use crate::{unreset, RESET_DMA};
use crate::{SRAM_BASE, SRAM_END};

pub const CHANNELS: u8 = 12;
#[cfg(any(target_arch = "arm", test))]
const CHANNEL_STRIDE: u32 = 0x40;
#[cfg(target_arch = "arm")]
const READ_ADDR: u32 = 0x00;
#[cfg(target_arch = "arm")]
const WRITE_ADDR: u32 = 0x04;
#[cfg(target_arch = "arm")]
const TRANS_COUNT: u32 = 0x08;
#[cfg(any(target_arch = "arm", test))]
const CTRL_TRIG: u32 = 0x0c;
#[cfg(any(target_arch = "arm", test))]
const INTR: u32 = 0x400;
#[cfg(any(target_arch = "arm", test))]
const INTE0: u32 = 0x404;
#[cfg(any(target_arch = "arm", test))]
const INTS0: u32 = 0x40c;
#[cfg(any(target_arch = "arm", test))]
const MULTI_CHAN_TRIGGER: u32 = 0x430;
#[cfg(any(target_arch = "arm", test))]
const CHAN_ABORT: u32 = 0x444;

const CTRL_ENABLE: u32 = 1;
const CTRL_INCR_READ: u32 = 1 << 4;
const CTRL_INCR_WRITE: u32 = 1 << 5;
const CTRL_CHAIN_TO_SHIFT: u32 = 11;
const CTRL_TREQ_SHIFT: u32 = 15;
#[cfg(test)]
const CTRL_BUSY: u32 = 1 << 24;
const CTRL_WRITE_ERROR: u32 = 1 << 29;
const CTRL_READ_ERROR: u32 = 1 << 30;
#[cfg(target_arch = "arm")]
const CTRL_AHB_ERROR: u32 = 1 << 31;
#[cfg(target_arch = "arm")]
const CTRL_ERRORS: u32 = CTRL_WRITE_ERROR | CTRL_READ_ERROR | CTRL_AHB_ERROR;

#[cfg(target_arch = "arm")]
static mut CLAIMED: u32 = 0;
#[cfg(target_arch = "arm")]
static mut GENERATION: [u32; CHANNELS as usize] = [0; CHANNELS as usize];
#[cfg(target_arch = "arm")]
static mut COMPLETION: [u32; CHANNELS as usize] = [0; CHANNELS as usize];

#[cfg(not(target_arch = "arm"))]
static CLAIMED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
#[cfg(not(target_arch = "arm"))]
static GENERATION: [core::sync::atomic::AtomicU32; CHANNELS as usize] =
    [const { core::sync::atomic::AtomicU32::new(0) }; CHANNELS as usize];
#[cfg(not(target_arch = "arm"))]
static COMPLETION: [core::sync::atomic::AtomicU32; CHANNELS as usize] =
    [const { core::sync::atomic::AtomicU32::new(0) }; CHANNELS as usize];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NoChannelFree,
    InvalidChannel,
    InvalidRange,
    InvalidCount,
    StaleChannel,
    CompletionBusy,
    HardwareFault,
}

#[cfg(target_arch = "arm")]
fn with_state<T>(
    f: impl FnOnce(&mut u32, &mut [u32; CHANNELS as usize], &mut [u32; CHANNELS as usize]) -> T,
) -> T {
    const DMA_STATE_LOCK: *mut u32 = (crate::SIO_BASE + 0x100 + 30 * 4) as *mut u32;
    let primask: u32;
    unsafe {
        core::arch::asm!(
            "mrs {state}, PRIMASK",
            "cpsid i",
            state = out(reg) primask,
            options(nomem, nostack)
        );
        while DMA_STATE_LOCK.read_volatile() == 0 {
            core::hint::spin_loop();
        }
        core::arch::asm!("dmb", options(nostack));
        let claimed_ptr = core::ptr::addr_of_mut!(CLAIMED);
        let generation_ptr = core::ptr::addr_of_mut!(GENERATION);
        let completion_ptr = core::ptr::addr_of_mut!(COMPLETION);
        let mut claimed = claimed_ptr.read_volatile();
        let mut generation = generation_ptr.read_volatile();
        let mut completion = completion_ptr.read_volatile();
        let result = f(&mut claimed, &mut generation, &mut completion);
        claimed_ptr.write_volatile(claimed);
        generation_ptr.write_volatile(generation);
        completion_ptr.write_volatile(completion);
        core::arch::asm!("dmb", options(nostack));
        DMA_STATE_LOCK.write_volatile(1);
        if primask & 1 == 0 {
            core::arch::asm!("cpsie i", options(nomem, nostack));
        }
        result
    }
}

fn claimed_mask() -> u32 {
    #[cfg(target_arch = "arm")]
    {
        with_state(|claimed, _, _| *claimed)
    }
    #[cfg(not(target_arch = "arm"))]
    {
        CLAIMED.load(core::sync::atomic::Ordering::Acquire)
    }
}

fn state_current(number: u8, generation: u32) -> bool {
    if number >= CHANNELS {
        return false;
    }
    let mask = 1 << number;
    #[cfg(target_arch = "arm")]
    {
        with_state(|claimed, generations, _| {
            *claimed & mask != 0 && generations[number as usize] == generation
        })
    }
    #[cfg(not(target_arch = "arm"))]
    {
        CLAIMED.load(core::sync::atomic::Ordering::Acquire) & mask != 0
            && GENERATION[number as usize].load(core::sync::atomic::Ordering::Acquire) == generation
    }
}

fn state_claim(number: u8) -> Result<u32, Error> {
    let mask = 1 << number;
    #[cfg(target_arch = "arm")]
    {
        with_state(|claimed, generations, completions| {
            if *claimed & mask != 0 {
                return Err(Error::NoChannelFree);
            }
            *claimed |= mask;
            completions[number as usize] = 0;
            generations[number as usize] = generations[number as usize].wrapping_add(1);
            Ok(generations[number as usize])
        })
    }
    #[cfg(not(target_arch = "arm"))]
    {
        use core::sync::atomic::Ordering;
        let mut claimed = CLAIMED.load(Ordering::Acquire);
        loop {
            if claimed & mask != 0 {
                return Err(Error::NoChannelFree);
            }
            match CLAIMED.compare_exchange_weak(
                claimed,
                claimed | mask,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => claimed = observed,
            }
        }
        COMPLETION[number as usize].store(0, Ordering::Release);
        Ok(GENERATION[number as usize]
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1))
    }
}

fn state_release(number: u8, generation: u32) -> bool {
    let mask = 1 << number;
    #[cfg(target_arch = "arm")]
    {
        with_state(|claimed, generations, completions| {
            if *claimed & mask == 0 || generations[number as usize] != generation {
                return false;
            }
            completions[number as usize] = 0;
            *claimed &= !mask;
            true
        })
    }
    #[cfg(not(target_arch = "arm"))]
    {
        use core::sync::atomic::Ordering;
        if !state_current(number, generation) {
            return false;
        }
        COMPLETION[number as usize].store(0, Ordering::Release);
        CLAIMED.fetch_and(!mask, Ordering::Release);
        true
    }
}

fn state_publish(number: u8, generation: u32, completion: u32) -> Result<(), Error> {
    if completion == 0 {
        return Err(Error::InvalidCount);
    }
    #[cfg(target_arch = "arm")]
    let mask = 1 << number;
    #[cfg(target_arch = "arm")]
    {
        with_state(|claimed, generations, completions| {
            if *claimed & mask == 0 || generations[number as usize] != generation {
                return Err(Error::StaleChannel);
            }
            if completions[number as usize] != 0 {
                return Err(Error::CompletionBusy);
            }
            completions[number as usize] = completion;
            Ok(())
        })
    }
    #[cfg(not(target_arch = "arm"))]
    {
        use core::sync::atomic::Ordering;
        if !state_current(number, generation) {
            return Err(Error::StaleChannel);
        }
        COMPLETION[number as usize]
            .compare_exchange(0, completion, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| Error::CompletionBusy)
    }
}

#[cfg(target_arch = "arm")]
fn state_take_completion(number: u8) -> u32 {
    with_state(|_, _, completions| {
        let completion = completions[number as usize];
        completions[number as usize] = 0;
        completion
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    MemoryToPeripheral,
    PeripheralToMemory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dreq(u8);

impl Dreq {
    pub const UART1_TX: Self = Self(22);
    pub const UART1_RX: Self = Self(23);
    pub const PERMANENT: Self = Self(0x3f);

    pub const fn number(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferConfig {
    pub direction: Direction,
    pub memory_addr: u32,
    pub peripheral_addr: u32,
    pub count: u32,
    pub dreq: Dreq,
}

impl TransferConfig {
    pub const fn memory_to_peripheral(
        memory_addr: u32,
        peripheral_addr: u32,
        count: u32,
        dreq: Dreq,
    ) -> Self {
        Self {
            direction: Direction::MemoryToPeripheral,
            memory_addr,
            peripheral_addr,
            count,
            dreq,
        }
    }

    pub const fn peripheral_to_memory(
        peripheral_addr: u32,
        memory_addr: u32,
        count: u32,
        dreq: Dreq,
    ) -> Self {
        Self {
            direction: Direction::PeripheralToMemory,
            memory_addr,
            peripheral_addr,
            count,
            dreq,
        }
    }
}

#[derive(Debug)]
pub struct Channel {
    number: u8,
    generation: u32,
    owned: bool,
}

impl Channel {
    pub const fn number(&self) -> u8 {
        self.number
    }

    fn current(&self) -> bool {
        self.owned && state_current(self.number, self.generation)
    }

    pub fn configure(&self, config: TransferConfig) -> Result<(), Error> {
        if !self.current() {
            return Err(Error::StaleChannel);
        }
        let ctrl = encode_control(self.number, config)?;
        #[cfg(not(target_arch = "arm"))]
        let _ = ctrl;
        #[cfg(target_arch = "arm")]
        unsafe {
            unreset(RESET_DMA);
            let base = channel_base(self.number);
            let (read, write) = match config.direction {
                Direction::MemoryToPeripheral => (config.memory_addr, config.peripheral_addr),
                Direction::PeripheralToMemory => (config.peripheral_addr, config.memory_addr),
            };
            reg(base + READ_ADDR).write_volatile(read);
            reg(base + WRITE_ADDR).write_volatile(write);
            reg(base + TRANS_COUNT).write_volatile(config.count);
            reg(base + CTRL_TRIG).write_volatile(ctrl);
        }
        Ok(())
    }

    pub fn enable_irq0(&self, enabled: bool) -> Result<(), Error> {
        if !self.current() {
            return Err(Error::StaleChannel);
        }
        #[cfg(not(target_arch = "arm"))]
        let _ = enabled;
        #[cfg(target_arch = "arm")]
        unsafe {
            let mask = 1 << self.number;
            let value = reg(DMA_BASE + INTE0).read_volatile();
            reg(DMA_BASE + INTE0).write_volatile(if enabled {
                value | mask
            } else {
                value & !mask
            });
        }
        Ok(())
    }

    /// Associate a non-zero completion token with this channel before it is
    /// started. The IRQ top-half takes the token when this channel completes.
    pub fn publish_completion(&self, completion: u32) -> Result<(), Error> {
        if !self.owned {
            return Err(Error::StaleChannel);
        }
        state_publish(self.number, self.generation, completion)
    }

    pub fn hardware_error(&self) -> Result<bool, Error> {
        if !self.current() {
            return Err(Error::StaleChannel);
        }
        #[cfg(target_arch = "arm")]
        unsafe {
            return Ok(
                reg(channel_base(self.number) + CTRL_TRIG).read_volatile() & CTRL_ERRORS != 0,
            );
        }
        #[cfg(not(target_arch = "arm"))]
        Ok(false)
    }

    pub fn cancel(&self) -> Result<(), Error> {
        if !self.current() {
            return Err(Error::StaleChannel);
        }
        #[cfg(target_arch = "arm")]
        unsafe {
            cleanup(self.number);
        }
        Ok(())
    }

    pub fn release(&mut self) -> Result<(), Error> {
        if !self.current() {
            return Err(Error::StaleChannel);
        }
        #[cfg(target_arch = "arm")]
        unsafe {
            cleanup(self.number);
        }
        if !state_release(self.number, self.generation) {
            return Err(Error::StaleChannel);
        }
        self.owned = false;
        Ok(())
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        if self.current() {
            let _ = self.release();
        }
    }
}

pub fn claim() -> Result<Channel, Error> {
    for number in 0..CHANNELS {
        if let Ok(channel) = claim_number(number) {
            return Ok(channel);
        }
    }
    Err(Error::NoChannelFree)
}

pub fn claim_number(number: u8) -> Result<Channel, Error> {
    if number >= CHANNELS {
        return Err(Error::InvalidChannel);
    }
    let generation = state_claim(number)?;
    #[cfg(target_arch = "arm")]
    unsafe {
        unreset(RESET_DMA);
        cleanup(number);
    }
    Ok(Channel {
        number,
        generation,
        owned: true,
    })
}

pub fn start_mask(channels: u32) -> Result<(), Error> {
    let valid = (1 << CHANNELS) - 1;
    if channels == 0 || channels & !valid != 0 || channels & claimed_mask() != channels {
        return Err(Error::InvalidChannel);
    }
    #[cfg(target_arch = "arm")]
    unsafe {
        reg(DMA_BASE + MULTI_CHAN_TRIGGER).write_volatile(channels);
    }
    Ok(())
}

pub fn take_irq0() -> u32 {
    #[cfg(target_arch = "arm")]
    unsafe {
        let pending = reg(DMA_BASE + INTS0).read_volatile() & ((1 << CHANNELS) - 1);
        reg(DMA_BASE + INTS0).write_volatile(pending);
        return pending;
    }
    #[cfg(not(target_arch = "arm"))]
    0
}

/// Acknowledge one IRQ0 channel and take the completion token it published.
/// Clearing one bit at a time leaves any concurrent completion asserted so the
/// NVIC re-enters and no channel's broker notification is lost.
pub fn take_irq0_completion() -> Option<u32> {
    #[cfg(target_arch = "arm")]
    unsafe {
        let pending = reg(DMA_BASE + INTS0).read_volatile() & ((1 << CHANNELS) - 1);
        if pending == 0 {
            return None;
        }
        let number = pending.trailing_zeros() as u8;
        reg(DMA_BASE + INTS0).write_volatile(1 << number);
        return match state_take_completion(number) {
            0 => None,
            completion => Some(completion),
        };
    }
    #[cfg(not(target_arch = "arm"))]
    None
}

fn encode_control(channel: u8, config: TransferConfig) -> Result<u32, Error> {
    if channel >= CHANNELS {
        return Err(Error::InvalidChannel);
    }
    if config.count == 0 {
        return Err(Error::InvalidCount);
    }
    if !range_within(config.memory_addr, config.count, SRAM_BASE, SRAM_END)
        || !range_within(config.peripheral_addr, 4, 0x4000_0000, 0x4008_0000)
    {
        return Err(Error::InvalidRange);
    }
    let increment = match config.direction {
        Direction::MemoryToPeripheral => CTRL_INCR_READ,
        Direction::PeripheralToMemory => CTRL_INCR_WRITE,
    };
    Ok(CTRL_ENABLE
        | CTRL_READ_ERROR
        | CTRL_WRITE_ERROR
        | increment
        | (u32::from(channel) << CTRL_CHAIN_TO_SHIFT)
        | (u32::from(config.dreq.number()) << CTRL_TREQ_SHIFT))
}

#[cfg(any(target_arch = "arm", test))]
const fn channel_base(channel: u8) -> u32 {
    DMA_BASE + channel as u32 * CHANNEL_STRIDE
}

#[cfg(target_arch = "arm")]
const fn reg(address: u32) -> *mut u32 {
    address as *mut u32
}

#[cfg(target_arch = "arm")]
unsafe fn cleanup(channel: u8) {
    let mask = 1 << channel;
    let enabled = reg(DMA_BASE + INTE0).read_volatile();
    reg(DMA_BASE + INTE0).write_volatile(enabled & !mask);
    reg(DMA_BASE + CHAN_ABORT).write_volatile(mask);
    while reg(DMA_BASE + CHAN_ABORT).read_volatile() & mask != 0 {
        core::hint::spin_loop();
    }
    // RP2040-E13 may deliver a completion after ABORT clears.
    reg(DMA_BASE + INTR).write_volatile(mask);
    reg(channel_base(channel) + CTRL_TRIG).write_volatile(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_words_match_pico_sdk_fields() {
        let tx =
            TransferConfig::memory_to_peripheral(SRAM_BASE, crate::UART1_BASE, 512, Dreq::UART1_TX);
        let rx = TransferConfig::peripheral_to_memory(
            crate::UART1_BASE,
            SRAM_BASE + 512,
            512,
            Dreq::UART1_RX,
        );
        assert_eq!(
            encode_control(2, tx).unwrap() & CTRL_INCR_READ,
            CTRL_INCR_READ
        );
        assert_eq!(
            encode_control(3, rx).unwrap() & CTRL_INCR_WRITE,
            CTRL_INCR_WRITE
        );
        assert_eq!(
            (encode_control(2, tx).unwrap() >> CTRL_TREQ_SHIFT) & 0x3f,
            22
        );
        assert_eq!(
            (encode_control(3, rx).unwrap() >> CTRL_TREQ_SHIFT) & 0x3f,
            23
        );
    }

    #[test]
    fn invalid_ranges_and_counts_are_rejected() {
        let peripheral = crate::UART1_BASE;
        assert_eq!(
            encode_control(
                0,
                TransferConfig::memory_to_peripheral(SRAM_BASE - 1, peripheral, 1, Dreq::UART1_TX)
            ),
            Err(Error::InvalidRange)
        );
        assert_eq!(
            encode_control(
                0,
                TransferConfig::memory_to_peripheral(SRAM_END - 1, peripheral, 2, Dreq::UART1_TX)
            ),
            Err(Error::InvalidRange)
        );
        assert_eq!(
            encode_control(
                0,
                TransferConfig::memory_to_peripheral(SRAM_BASE, peripheral, 0, Dreq::UART1_TX)
            ),
            Err(Error::InvalidCount)
        );
    }

    #[test]
    fn double_claim_and_stale_channel_are_rejected() {
        let mut first = claim_number(11).unwrap();
        assert!(matches!(claim_number(11), Err(Error::NoChannelFree)));
        first.publish_completion(7).unwrap();
        assert_eq!(first.publish_completion(8), Err(Error::CompletionBusy));
        first.release().unwrap();
        assert_eq!(
            first.configure(TransferConfig::memory_to_peripheral(
                SRAM_BASE,
                crate::UART1_BASE,
                1,
                Dreq::UART1_TX,
            )),
            Err(Error::StaleChannel)
        );
        assert_eq!(first.publish_completion(9), Err(Error::StaleChannel));
        let mut second = claim_number(11).unwrap();
        second.release().unwrap();
    }

    #[test]
    fn constants_match_rp2040_register_layout() {
        assert_eq!(DMA_BASE, 0x5000_0000);
        assert_eq!(channel_base(11) + CTRL_TRIG, 0x5000_02cc);
        assert_eq!(
            (INTR, INTE0, INTS0, MULTI_CHAN_TRIGGER, CHAN_ABORT),
            (0x400, 0x404, 0x40c, 0x430, 0x444)
        );
        assert_eq!((CTRL_BUSY, CTRL_ENABLE), (1 << 24, 1));
    }
}
