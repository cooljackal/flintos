// SPDX-License-Identifier: Apache-2.0

//! The shared DMA channels, and who owns them.
//!
//! The ESP32 has **no general-purpose DMA controller** — that arrived with the
//! S3's GDMA. What it has is three channels behind a crossbar, shared by SPI1,
//! SPI2 and SPI3, selected by a 2-bit field per host in one DPORT register.
//!
//! Two drivers each assuming they own a channel do not fail; they write each
//! other's descriptors and corrupt each other's transfers. Nothing in the
//! hardware objects. So the point of this module is that the *second* claim
//! returns an error.
//!
//! # DMA is a per-chip thing, not a portable one
//!
//! This module is `soc-esp32` and nothing about it generalises. The ESP32 has
//! a three-channel crossbar for SPI; the S3 has a GDMA with a completely
//! different allocation model; an STM32 has numbered streams with fixed
//! peripheral mappings; an RP2040 has twelve uniform channels. There is no
//! shared vocabulary worth inventing until a second chip is actually here to
//! say what it needs, so there is no `hal` trait for this and drivers name the
//! SoC's allocator directly.
//!
//! What *is* portable is the discipline: buffers and descriptors must live
//! where the engine can reach them, and that is `kernel::dma_broker`.
//!
//! # What is not in this crossbar
//!
//! I2S0 and I2S1 each have their own dedicated DMA engine on this chip, and so
//! do SDMMC and UHCI. They are not allocated from here and do not contend with
//! SPI. (Issue #18 lists I2S as sharing these channels; that is true of some
//! later parts and not of the ESP32 v1.)
//!
//! # Register facts
//!
//! `DPORT_SPI_DMA_CHAN_SEL_REG` at `DPORT_BASE + 0x5A8`, from `dport_reg.h`:
//!
//! | Host | Field |
//! |---|---|
//! | SPI1 | bits [1:0] |
//! | SPI2 | bits [3:2] |
//! | SPI3 | bits [5:4] |
//!
//! A field of 0 means "no channel"; 1, 2 and 3 select the channel of that
//! number. There is no channel 0, which is why [`Channel`] starts at 1.

use core::sync::atomic::{AtomicU8, Ordering};

use crate::addr::DPORT_BASE;

// The descriptor layer. Its own file because it is a separate concern from
// owning the crossbar -- one says who may use a channel, the other says what
// the engine reads once they have it -- and together they would bury both.
#[path = "dma_desc.rs"]
mod desc;

pub use desc::{
    build_chain, build_ring, descriptors_needed, link_addr, reachable, received_len, ring_slot,
    Descriptor, Direction,
};

/// This SoC's [`hal::dma::DmaReach`] — the ESP32's DMA-reachable window is all
/// internal DRAM. A driver holds one of these to validate a buffer without
/// naming the address range itself.
pub struct DmaReach;

impl hal::dma::DmaReach for DmaReach {
    fn reachable(&self, addr: u32, len: u32) -> bool {
        if len == 0 {
            return true;
        }
        // Both ends must land inside the window; `reachable` is an inclusive
        // lower / exclusive upper check, so the last byte is `addr + len - 1`.
        reachable(addr) && reachable(addr + len - 1)
    }
}

/// Publish descriptor and buffer writes before a DMA link is started.
///
/// Xtensa's write buffer can otherwise leave the peripheral observing the
/// previous owner bit when a descriptor is rebuilt and immediately re-armed.
#[inline(always)]
pub fn sync_for_device() {
    #[cfg(target_arch = "xtensa")]
    unsafe {
        core::arch::asm!("memw", options(nostack, preserves_flags));
    }
    #[cfg(not(target_arch = "xtensa"))]
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
}

/// `DPORT_SPI_DMA_CHAN_SEL_REG`.
const SPI_DMA_CHAN_SEL: u32 = DPORT_BASE + 0x5A8;

/// Each host's selector is 2 bits.
const SEL_MASK: u32 = 0x3;

/// How many channels the crossbar has. Numbered 1..=3.
pub const CHANNELS: u8 = 3;

/// A SPI host that can be given a DMA channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// SPI1 — shares pins with the flash on most boards. Included because the
    /// crossbar has a field for it, not because using it is a good idea.
    Spi1,
    Spi2,
    Spi3,
}

impl Host {
    /// Bit position of this host's selector field.
    const fn shift(self) -> u32 {
        match self {
            Host::Spi1 => 0,
            Host::Spi2 => 2,
            Host::Spi3 => 4,
        }
    }
}

/// A claimed DMA channel.
///
/// Not `Clone` or `Copy`: two owners is exactly the situation this module
/// exists to prevent, and the type should not hand one out.
#[derive(Debug, PartialEq, Eq)]
pub struct Channel {
    number: u8,
    host: Host,
}

impl Channel {
    /// The channel number, 1..=3, as the hardware numbers them.
    pub const fn number(&self) -> u8 {
        self.number
    }

    /// Which host this channel is wired to.
    pub const fn host(&self) -> Host {
        self.host
    }
}

/// Why a claim failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    /// Every channel is already claimed.
    NoChannelFree,
    /// This host already holds a channel. Releasing it first is the caller's
    /// job — silently handing out a second would leave the first leaked and
    /// the crossbar pointing at only one of them.
    HostAlreadyHasChannel,
    /// A descriptor was asked to carry more than [`Descriptor::MAX_LEN`].
    ChunkTooLong,
    /// A buffer or descriptor address the engine cannot use: outside the
    /// DMA-reachable region, not word-aligned, or beyond the 20 bits the link
    /// register can hold.
    UnreachableAddress,
    /// The caller supplied too few descriptors for the buffer.
    NotEnoughDescriptors,
}

/// Which host owns each channel, indexed by `channel - 1`. `0` means free.
///
/// An atomic per channel rather than a lock: a claim is one compare-exchange,
/// and this is reachable from a driver's init path on either core.
static OWNER: [AtomicU8; CHANNELS as usize] = [const { AtomicU8::new(0) }; CHANNELS as usize];

/// Encode a host as a non-zero owner tag.
const fn tag(host: Host) -> u8 {
    match host {
        Host::Spi1 => 1,
        Host::Spi2 => 2,
        Host::Spi3 => 3,
    }
}

/// Claim a free channel for `host` and point the crossbar at it.
///
/// # Safety
/// Writes `DPORT_SPI_DMA_CHAN_SEL_REG`. The returned channel is exclusively
/// the caller's until it is [`release`]d.
pub unsafe fn claim(host: Host) -> Result<Channel, DmaError> {
    let want = tag(host);

    // A host with a channel already must not get a second: the crossbar has
    // one field per host, so the first channel would stay marked owned with
    // nothing pointing at it.
    if OWNER.iter().any(|o| o.load(Ordering::Acquire) == want) {
        return Err(DmaError::HostAlreadyHasChannel);
    }

    for (i, owner) in OWNER.iter().enumerate() {
        if owner
            .compare_exchange(0, want, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            let number = i as u8 + 1;
            set_selector(host, number);
            return Ok(Channel { number, host });
        }
    }
    Err(DmaError::NoChannelFree)
}

/// Give a channel back and disconnect it from its host.
///
/// # Safety
/// Writes `DPORT_SPI_DMA_CHAN_SEL_REG`. Any transfer still in flight on this
/// channel is abandoned, not stopped — stop the peripheral first.
pub unsafe fn release(channel: Channel) {
    // Disconnect before freeing. The other order leaves a window in which
    // another host can claim the channel while the crossbar still routes it to
    // the old one.
    set_selector(channel.host, 0);
    OWNER[(channel.number - 1) as usize].store(0, Ordering::Release);
}

/// Point `host`'s selector at `channel` (0 disconnects).
///
/// # Safety
/// Read-modify-write of a DPORT register shared with the other hosts' fields.
unsafe fn set_selector(host: Host, channel: u8) {
    let shift = host.shift();
    // Through `dport::modify`, not a bare read-modify-write: this register
    // holds all three hosts' selectors, so two cores claiming different
    // channels race on it, and the read half is subject to the DPORT erratum.
    crate::dport::modify(
        SPI_DMA_CHAN_SEL,
        SEL_MASK << shift,
        (channel as u32 & SEL_MASK) << shift,
    );
}

/// How many channels are unclaimed.
pub fn free_channels() -> u8 {
    OWNER
        .iter()
        .filter(|o| o.load(Ordering::Acquire) == 0)
        .count() as u8
}

/// Which host owns `channel` (1..=3), if any.
pub fn owner_of(channel: u8) -> Option<Host> {
    if channel == 0 || channel > CHANNELS {
        return None;
    }
    match OWNER[(channel - 1) as usize].load(Ordering::Acquire) {
        1 => Some(Host::Spi1),
        2 => Some(Host::Spi2),
        3 => Some(Host::Spi3),
        _ => None,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hal::dma::DmaReach as DmaReachTrait;

    #[test]
    fn dma_reach_requires_both_ends_in_the_window() {
        let r = DmaReach;
        // Wholly inside internal DRAM.
        assert!(r.reachable(0x3FFB_0000, 256));
        // Starts one byte before the window.
        assert!(!r.reachable(0x3FFA_DFFF, 4));
        // Ends one byte past the window (last byte == 0x4000_0000).
        assert!(!r.reachable(0x3FFF_FFFE, 4));
        // A range ending exactly at the last reachable byte is fine.
        assert!(r.reachable(0x3FFF_FFF0, 16));
        // Zero length is vacuously reachable even at a bad address.
        assert!(r.reachable(0x4008_0000, 0));
        // Entirely outside (IRAM).
        assert!(!r.reachable(0x4008_0000, 64));
    }

    /// The register write is the only part that needs hardware; the ownership
    /// bookkeeping is what these check, so they run the allocator directly and
    /// reset it between tests.
    fn reset() {
        for o in &OWNER {
            o.store(0, Ordering::Release);
        }
    }

    /// Claim without touching the crossbar register.
    fn claim_bookkeeping(host: Host) -> Result<Channel, DmaError> {
        let want = tag(host);
        if OWNER.iter().any(|o| o.load(Ordering::Acquire) == want) {
            return Err(DmaError::HostAlreadyHasChannel);
        }
        for (i, owner) in OWNER.iter().enumerate() {
            if owner
                .compare_exchange(0, want, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return Ok(Channel { number: i as u8 + 1, host });
            }
        }
        Err(DmaError::NoChannelFree)
    }

    fn release_bookkeeping(c: Channel) {
        OWNER[(c.number - 1) as usize].store(0, Ordering::Release);
    }

    #[test]
    fn the_selector_fields_match_the_header() {
        assert_eq!(SPI_DMA_CHAN_SEL, 0x3FF0_05A8);
        assert_eq!(Host::Spi1.shift(), 0);
        assert_eq!(Host::Spi2.shift(), 2);
        assert_eq!(Host::Spi3.shift(), 4);
        // Two bits each, so the three fields must not run into one another.
        assert_eq!(SEL_MASK, 0x3);
        for (a, b) in [(Host::Spi1, Host::Spi2), (Host::Spi2, Host::Spi3)] {
            assert_eq!(b.shift() - a.shift(), 2, "fields overlap");
        }
    }

    #[test]
    fn a_channel_is_numbered_from_one() {
        // Zero means "no channel" in the register, so there is no channel 0.
        // An allocator handing out 0 would disconnect the host it just served.
        let _l = crate::test_lock();
        reset();
        let c = claim_bookkeeping(Host::Spi2).unwrap();
        assert!((1..=CHANNELS).contains(&c.number()));
        release_bookkeeping(c);
    }

    #[test]
    fn a_second_host_gets_a_different_channel() {
        let _l = crate::test_lock();
        reset();
        let a = claim_bookkeeping(Host::Spi2).unwrap();
        let b = claim_bookkeeping(Host::Spi3).unwrap();
        assert_ne!(a.number(), b.number(), "two hosts on one channel");
        release_bookkeeping(a);
        release_bookkeeping(b);
    }

    #[test]
    fn claiming_twice_for_one_host_is_refused() {
        // The crossbar has one field per host, so a second channel would be
        // marked owned with nothing routed to it -- leaked, silently.
        let _l = crate::test_lock();
        reset();
        let a = claim_bookkeeping(Host::Spi2).unwrap();
        assert_eq!(claim_bookkeeping(Host::Spi2), Err(DmaError::HostAlreadyHasChannel));
        release_bookkeeping(a);
    }

    #[test]
    fn a_fourth_claim_is_refused_rather_than_shared() {
        // Three channels, three hosts. The failure this replaces is two
        // drivers quietly writing each other's descriptors.
        let _l = crate::test_lock();
        reset();
        let a = claim_bookkeeping(Host::Spi1).unwrap();
        let b = claim_bookkeeping(Host::Spi2).unwrap();
        let c = claim_bookkeeping(Host::Spi3).unwrap();
        assert_eq!(free_channels(), 0);
        // Every host already holds one, so this is the host check; free the
        // first and the pool is genuinely empty for a repeat claim.
        release_bookkeeping(a);
        let a2 = claim_bookkeeping(Host::Spi1).unwrap();
        assert_eq!(free_channels(), 0);
        release_bookkeeping(a2);
        release_bookkeeping(b);
        release_bookkeeping(c);
        assert_eq!(free_channels(), CHANNELS);
    }

    #[test]
    fn a_released_channel_comes_back() {
        let _l = crate::test_lock();
        reset();
        let before = free_channels();
        let c = claim_bookkeeping(Host::Spi3).unwrap();
        assert_eq!(free_channels(), before - 1);
        release_bookkeeping(c);
        assert_eq!(free_channels(), before);
    }

    #[test]
    fn ownership_is_reported_per_channel() {
        let _l = crate::test_lock();
        reset();
        let c = claim_bookkeeping(Host::Spi2).unwrap();
        assert_eq!(owner_of(c.number()), Some(Host::Spi2));
        assert_eq!(owner_of(0), None, "there is no channel 0");
        assert_eq!(owner_of(CHANNELS + 1), None);
        release_bookkeeping(c);
        assert_eq!(owner_of(1), None);
    }

    #[test]
    fn the_selector_encoding_isolates_each_host() {
        // Emulate the register write, because getting the mask wrong would
        // disconnect a neighbouring host rather than failing visibly.
        let mut reg = 0u32;
        for (host, ch) in [(Host::Spi1, 1u32), (Host::Spi2, 2), (Host::Spi3, 3)] {
            let shift = host.shift();
            reg = (reg & !(SEL_MASK << shift)) | (ch << shift);
        }
        assert_eq!(reg & 0x3, 1, "SPI1");
        assert_eq!((reg >> 2) & 0x3, 2, "SPI2");
        assert_eq!((reg >> 4) & 0x3, 3, "SPI3");

        // Disconnecting one must leave the others alone.
        let shift = Host::Spi2.shift();
        reg &= !(SEL_MASK << shift);
        assert_eq!(reg & 0x3, 1, "SPI1 disturbed");
        assert_eq!((reg >> 2) & 0x3, 0, "SPI2 not cleared");
        assert_eq!((reg >> 4) & 0x3, 3, "SPI3 disturbed");
    }
}
