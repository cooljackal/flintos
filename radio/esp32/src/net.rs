// SPDX-License-Identifier: Apache-2.0

//! The smoltcp link layer for an associated Wi-Fi station.
//!
//! Phase 6 of `doc/plan-radio.md`, issue #68. `smoltcp` is the TCP/IP stack —
//! Rust, `no_std`, and heapless: it never allocates on its own, working instead
//! from buffers we hand it. That suits ground rule 3, where only this tier
//! allocates and the kernel stays static; every buffer here comes from the
//! radio heap ([`kernel::heap`]).
//!
//! # What this file is
//!
//! The **device seam** — the boundary smoltcp's [`phy::Device`] trait sits on.
//! An associated station moves raw Ethernet frames two ways:
//!
//! - **Transmit** hands a frame straight to the blob's `esp_wifi_internal_tx`,
//!   the same raw-L2 path the 4-way handshake sends EAPOL through
//!   ([`crate::supplicant`]). smoltcp writes the frame into the token's buffer;
//!   [`TxToken::consume`] passes it down.
//! - **Receive** drains an [`RxQueue`] the MAC's receive callback fills. Each
//!   frame the blob delivers is copied into a free ring slot on the driver's
//!   task; [`phy::Device::receive`] hands the oldest one to smoltcp.
//!
//! # Status
//!
//! **6.1: the seam compiles into the tree.** The [`RxQueue`] and the
//! [`WifiDevice`] are here and host-tested for their ring behaviour, and
//! transmit is wired to the blob. What is **not** here yet, and not on
//! hardware, is 6.2's other half: registering the MAC receive callback that
//! fills the queue, and driving a smoltcp `Interface` poll loop. Those land
//! next, validated against a real AP — a device seam that has never carried a
//! frame is exactly the kind of thing this crate does not claim works until the
//! blob has driven it.

use core::sync::atomic::{AtomicUsize, Ordering};

use kernel::smp::Spinlock;
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::time::Instant;

/// Largest Ethernet II frame smoltcp will build or accept, headers included and
/// FCS excluded (the MAC appends and strips the FCS). 14-byte header + 1500 MTU.
pub const MTU: usize = 1514;

/// How many received frames the queue holds before it drops the oldest. Sized
/// to ride out a poll interval's worth of arrivals without stalling the MAC;
/// 16 × [`MTU`] is ~24 KiB out of the radio heap.
pub const RX_SLOTS: usize = 16;

/// `WIFI_IF_STA` — the interface index the station's frames ride, matching
/// [`crate::supplicant`]'s `IF_STA`.
#[cfg(target_os = "none")]
const IF_STA: u32 = 0;

#[cfg(target_os = "none")]
extern "C" {
    /// `int esp_wifi_internal_tx(wifi_interface_t, void*, uint16_t)`. Declared
    /// in [`crate::supplicant`]; redeclared here so this module stands alone.
    /// Sends a raw L2 frame — the caller supplies the Ethernet header.
    fn esp_wifi_internal_tx(ifx: u32, buffer: *const core::ffi::c_void, len: u16) -> i32;
}

/// One frame's worth of storage: the bytes plus how many are valid.
struct Slot {
    len: usize,
    bytes: [u8; MTU],
}

impl Slot {
    const fn new() -> Self {
        Self { len: 0, bytes: [0; MTU] }
    }
}

/// A single-producer/single-consumer ring of received frames.
///
/// The MAC receive callback is the producer, on the driver's task; the
/// interface poll is the consumer, on the network task. A full ring drops the
/// incoming frame rather than blocking the MAC — TCP recovers from a loss, a
/// wedged receive path does not. The [`Spinlock`] is held only for the index
/// arithmetic and the copy, never across a call out.
pub struct RxQueue {
    slots: Spinlock<[Slot; RX_SLOTS]>,
    /// Next slot the producer writes; only the producer advances it.
    head: AtomicUsize,
    /// Next slot the consumer reads; only the consumer advances it.
    tail: AtomicUsize,
    /// Frames dropped because the ring was full when they arrived. A counter,
    /// not an error: it is diagnostic, read back when a link looks lossy.
    dropped: AtomicUsize,
}

impl RxQueue {
    /// An empty queue. `const`, so it can back a `static` and be filled by the
    /// receive callback without a heap allocation for the ring itself.
    #[allow(clippy::declare_interior_mutable_const)]
    pub const fn new() -> Self {
        const EMPTY: Slot = Slot::new();
        Self {
            slots: Spinlock::new([EMPTY; RX_SLOTS]),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        }
    }

    /// Copy a received frame into the ring. Called by the MAC receive callback.
    /// Returns `false` and bumps [`Self::dropped`] if the ring is full or the
    /// frame is too long to be one this MAC should have delivered.
    pub fn push(&self, frame: &[u8]) -> bool {
        if frame.len() > MTU {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let head = self.head.load(Ordering::Relaxed);
        let next = (head + 1) % RX_SLOTS;
        if next == self.tail.load(Ordering::Acquire) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        self.slots.with(|slots| {
            let slot = &mut slots[head];
            slot.bytes[..frame.len()].copy_from_slice(frame);
            slot.len = frame.len();
        });
        self.head.store(next, Ordering::Release);
        true
    }

    /// Take the oldest frame out, calling `f` with its bytes. Returns `None`
    /// when the ring is empty.
    fn pop_with<R>(&self, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let tail = self.tail.load(Ordering::Relaxed);
        if tail == self.head.load(Ordering::Acquire) {
            return None;
        }
        let r = self.slots.with(|slots| {
            let slot = &slots[tail];
            f(&slot.bytes[..slot.len])
        });
        self.tail.store((tail + 1) % RX_SLOTS, Ordering::Release);
        Some(r)
    }

    /// How many frames the ring has dropped, cumulative.
    pub fn dropped(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Default for RxQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// smoltcp's view of the associated station: an Ethernet device whose transmit
/// goes to the blob and whose receive drains an [`RxQueue`].
///
/// Zero-copy is not worth it here — the blob owns the receive buffer only for
/// the duration of its callback, so a frame is copied into the ring on arrival
/// regardless. The device borrows the queue rather than owning it: the queue is
/// a `static` the receive callback also names.
pub struct WifiDevice<'q> {
    rx: &'q RxQueue,
}

impl<'q> WifiDevice<'q> {
    /// Wrap a receive queue as a smoltcp device.
    pub fn new(rx: &'q RxQueue) -> Self {
        Self { rx }
    }
}

impl phy::Device for WifiDevice<'_> {
    type RxToken<'a>
        = WifiRxToken
    where
        Self: 'a;
    type TxToken<'a>
        = WifiTxToken
    where
        Self: 'a;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU;
        // One frame in flight: transmit hands straight to the blob, which has
        // its own queueing, so smoltcp need not burst.
        caps.max_burst_size = Some(1);
        caps
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // A frame smoltcp receives may prompt an immediate reply (an ARP or a
        // TCP ACK), so a receive yields a transmit token alongside it.
        let mut frame = [0u8; MTU];
        let len = self.rx.pop_with(|bytes| {
            frame[..bytes.len()].copy_from_slice(bytes);
            bytes.len()
        })?;
        Some((WifiRxToken { frame, len }, WifiTxToken))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(WifiTxToken)
    }
}

/// A received frame, owned by the token for the length of the `consume` call.
pub struct WifiRxToken {
    frame: [u8; MTU],
    len: usize,
}

impl phy::RxToken for WifiRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.frame[..self.len])
    }
}

/// The right to transmit one frame: smoltcp fills the buffer, and `consume`
/// hands it to the blob.
pub struct WifiTxToken;

impl phy::TxToken for WifiTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut frame = [0u8; MTU];
        let capped = len.min(MTU);
        let result = f(&mut frame[..capped]);
        tx_raw(&frame[..capped]);
        result
    }
}

/// Hand a built frame to the MAC. The blob copies it out synchronously, so the
/// borrow ends when this returns.
#[cfg(target_os = "none")]
fn tx_raw(frame: &[u8]) {
    // SAFETY: the blob reads `len` bytes from `frame` and copies them before
    // returning; the pointer is valid for that read and outlives the call.
    let rc = unsafe {
        esp_wifi_internal_tx(
            IF_STA,
            frame.as_ptr() as *const core::ffi::c_void,
            frame.len() as u16,
        )
    };
    if rc != 0 {
        // A transmit failure is the link's problem to recover from, not this
        // seam's; smoltcp will retransmit. Recorded, not propagated.
        api::log_debug!("wifi tx dropped a frame: rc={}", rc);
    }
}

/// Host stand-in: there is no blob to transmit through. The frame is dropped,
/// which is what lets the ring and token logic be exercised off-target.
#[cfg(not(target_os = "none"))]
fn tx_raw(_frame: &[u8]) {}

#[cfg(all(test, not(target_os = "none")))]
mod tests {
    use super::*;

    #[test]
    fn a_pushed_frame_comes_back_out_unchanged() {
        let q = RxQueue::new();
        assert!(q.push(&[1, 2, 3, 4]));
        let mut out = [0u8; MTU];
        let n = q.pop_with(|b| {
            out[..b.len()].copy_from_slice(b);
            b.len()
        });
        assert_eq!(n, Some(4));
        assert_eq!(&out[..4], &[1, 2, 3, 4]);
        // Drained: nothing left.
        assert!(q.pop_with(|_| ()).is_none());
    }

    #[test]
    fn the_ring_holds_one_fewer_than_its_slots_and_drops_the_rest() {
        let q = RxQueue::new();
        // A slot is spent on the full/empty distinction, so capacity is N-1.
        for i in 0..RX_SLOTS - 1 {
            assert!(q.push(&[i as u8]), "slot {i} should accept");
        }
        assert!(!q.push(&[0xff]), "the ring should now be full");
        assert_eq!(q.dropped(), 1);
        // The oldest frame is the first one pushed.
        assert_eq!(q.pop_with(|b| b[0]), Some(0));
    }

    #[test]
    fn an_oversize_frame_is_refused_not_truncated() {
        let q = RxQueue::new();
        let big = [0u8; MTU + 1];
        assert!(!q.push(&big));
        assert_eq!(q.dropped(), 1);
        assert!(q.pop_with(|_| ()).is_none());
    }

    #[test]
    fn the_ring_wraps_and_keeps_fifo_order() {
        let q = RxQueue::new();
        // Fill, drain, and refill so head and tail wrap past the end.
        for round in 0..3 {
            for i in 0..RX_SLOTS - 1 {
                assert!(q.push(&[(round * 10 + i) as u8]));
            }
            for i in 0..RX_SLOTS - 1 {
                assert_eq!(q.pop_with(|b| b[0]), Some((round * 10 + i) as u8));
            }
        }
    }
}
