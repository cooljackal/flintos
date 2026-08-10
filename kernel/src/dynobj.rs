// SPDX-License-Identifier: Apache-2.0

//! Runtime-created kernel objects, for the radio blobs.
//!
//! # A second object model, beside the first
//!
//! FlintOS's primitives are compile-time sized. `Queue<T, N>` fixes length and
//! type at build time, which is exactly right for a static RTOS and is what
//! gives it bounded latency and no fragmentation.
//!
//! Espressif's blobs do not work that way. They create and destroy objects at
//! runtime, in sizes they choose, through `wifi_osi_funcs_t`:
//! `queue_create(len, item_size)` copies bytes and does not know a type.
//!
//! This is not an adapter over the static primitives — it is a second model
//! standing beside them. The static ones keep their semantics and their tests
//! unchanged, and nothing here is meant for kernel or application use.
//!
//! # What is reused
//!
//! The blocking machinery, all of it. [`crate::queue`]'s waiter lists are keyed
//! by an arbitrary `usize`, not by a queue type, so a dynamic object passes its
//! own address and gets the scheduler integration, the timeout handling, the
//! refusal to block in interrupt context and — most valuable — the existing
//! race tests, which were hard won.
//!
//! What is *not* reused is storage: these live in [`crate::heap`], which exists
//! for the radio and is confined to it.
//!
//! # Why this is P1 and not routine
//!
//! Dynamic objects are where an RTOS loses its timing guarantees. Every
//! operation here is O(1) apart from waking waiters, which is the same cost the
//! static queues already pay. Nothing allocates on the send or receive path;
//! allocation happens at create and free at delete, and both are documented as
//! unsuitable for a hot path.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::heap::{self, Caps};
use crate::queue::{block_recv, block_send, wake_one_receiver, wake_one_sender};

/// Wait forever. Matches the convention the static queues already use.
pub const FOREVER: u32 = u32::MAX;

/// A byte-copying queue whose length and item size are chosen at runtime.
///
/// The blob's `queue_create(len, item_size)`. Items are opaque bytes: the
/// queue never knows a type, so it cannot drop one, which is why
/// [`DynQueue::delete`] is safe to call with items still in it.
pub struct DynQueue {
    /// Ring storage, `capacity * item_size` bytes from the radio heap.
    buf: *mut u8,
    capacity: usize,
    item_size: usize,
    head: usize,
    tail: usize,
    len: usize,
}

// Every field is behind the scheduler's critical section in use; the raw
// pointer is owned storage. Same reasoning as `heap::Heap`.
unsafe impl Send for DynQueue {}

impl DynQueue {
    /// Create a queue holding `capacity` items of `item_size` bytes.
    ///
    /// Returns `None` if the heap cannot supply the storage, or if either
    /// dimension is zero — a queue that can hold nothing is a bug at the call
    /// site, not something to model.
    pub fn create(capacity: usize, item_size: usize) -> Option<Self> {
        if capacity == 0 || item_size == 0 {
            return None;
        }
        let bytes = capacity.checked_mul(item_size)?;
        // Word alignment: the blob hands us item sizes that are often 4 and
        // copies through them as words.
        let buf = unsafe { heap::alloc(bytes, 4) };
        if buf.is_null() {
            return None;
        }
        Some(Self { buf, capacity, item_size, head: 0, tail: 0, len: 0 })
    }

    /// Items currently queued.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_full(&self) -> bool {
        self.len == self.capacity
    }

    /// Bytes per item, as given at creation.
    pub fn item_size(&self) -> usize {
        self.item_size
    }

    /// The key this queue's waiters are listed under.
    fn key(&self) -> usize {
        self as *const _ as usize
    }

    /// Copy one item in without blocking. `false` if full.
    ///
    /// # Safety
    /// `item` must point to at least [`DynQueue::item_size`] readable bytes.
    pub unsafe fn try_send(&mut self, item: *const u8) -> bool {
        if self.is_full() {
            return false;
        }
        let slot = unsafe { self.buf.add(self.tail * self.item_size) };
        unsafe { core::ptr::copy_nonoverlapping(item, slot, self.item_size) };
        self.tail = (self.tail + 1) % self.capacity;
        self.len += 1;
        true
    }

    /// Copy one item out without blocking. `false` if empty.
    ///
    /// # Safety
    /// `out` must point to at least [`DynQueue::item_size`] writable bytes.
    pub unsafe fn try_recv(&mut self, out: *mut u8) -> bool {
        if self.is_empty() {
            return false;
        }
        let slot = unsafe { self.buf.add(self.head * self.item_size) };
        unsafe { core::ptr::copy_nonoverlapping(slot, out, self.item_size) };
        self.head = (self.head + 1) % self.capacity;
        self.len -= 1;
        true
    }

    /// Send, blocking up to `timeout_ms` for a slot.
    ///
    /// # Safety
    /// As [`DynQueue::try_send`].
    pub unsafe fn send(&mut self, item: *const u8, timeout_ms: u32) -> bool {
        loop {
            if unsafe { self.try_send(item) } {
                wake_one_receiver(self.key());
                return true;
            }
            // `block_send` refuses in interrupt context and on timeout, which
            // is what stops this looping forever.
            if !block_send(self.key(), timeout_ms) {
                return false;
            }
        }
    }

    /// Receive, blocking up to `timeout_ms` for an item.
    ///
    /// # Safety
    /// As [`DynQueue::try_recv`].
    pub unsafe fn recv(&mut self, out: *mut u8, timeout_ms: u32) -> bool {
        loop {
            if unsafe { self.try_recv(out) } {
                wake_one_sender(self.key());
                return true;
            }
            if !block_recv(self.key(), timeout_ms) {
                return false;
            }
        }
    }

    /// Send from an interrupt handler. Never blocks.
    ///
    /// Returns `(sent, woke_higher_priority)`. The blob uses the second value
    /// to decide whether to yield on the way out of the handler.
    ///
    /// # Safety
    /// As [`DynQueue::try_send`].
    pub unsafe fn send_from_isr(&mut self, item: *const u8) -> (bool, bool) {
        if !unsafe { self.try_send(item) } {
            return (false, false);
        }
        wake_one_receiver(self.key());
        // Conservative: report that a yield may be worthwhile whenever a
        // receiver was waiting. Saying "no" wrongly costs a whole tick of
        // latency; saying "yes" wrongly costs one reschedule.
        (true, true)
    }

    /// Release the storage.
    ///
    /// Items still queued are simply discarded — they are bytes, and this type
    /// has never known how to drop one.
    pub fn delete(self) {
        unsafe { heap::free(self.buf, Caps::Internal) };
    }
}

/// A counting semaphore. Nothing like it exists in the static model.
///
/// A binary semaphore is this with a maximum of one, which is what the blob
/// asks for most often.
pub struct Semaphore {
    count: u32,
    max: u32,
}

impl Semaphore {
    /// Create with `initial` permits and a ceiling of `max`.
    pub fn create(max: u32, initial: u32) -> Option<Self> {
        if max == 0 || initial > max {
            return None;
        }
        Some(Self { count: initial, max })
    }

    /// A binary semaphore, created empty — the blob's usual "signal me" case.
    pub fn binary() -> Self {
        Self { count: 0, max: 1 }
    }

    pub fn count(&self) -> u32 {
        self.count
    }

    fn key(&self) -> usize {
        self as *const _ as usize
    }

    /// Take a permit without blocking.
    pub fn try_take(&mut self) -> bool {
        if self.count == 0 {
            return false;
        }
        self.count -= 1;
        true
    }

    /// Take a permit, blocking up to `timeout_ms`.
    pub fn take(&mut self, timeout_ms: u32) -> bool {
        loop {
            if self.try_take() {
                return true;
            }
            if !block_recv(self.key(), timeout_ms) {
                return false;
            }
        }
    }

    /// Return a permit. `false` if already at `max`, which is a caller bug
    /// rather than something to saturate silently.
    pub fn give(&mut self) -> bool {
        if self.count >= self.max {
            return false;
        }
        self.count += 1;
        wake_one_receiver(self.key());
        true
    }

    /// Give from an interrupt handler. Returns `(gave, woke_higher_priority)`.
    pub fn give_from_isr(&mut self) -> (bool, bool) {
        if !self.give() {
            return (false, false);
        }
        (true, true)
    }
}

/// A mutex that the same task may lock more than once.
///
/// **A second type, deliberately.** [`crate::mutex`] refuses re-entry and logs
/// an error, for a stated reason: nothing in its guard and unlock bookkeeping
/// supports nested ownership. That decision stands. The blob needs recursion,
/// so it gets its own type rather than the existing one being weakened.
///
/// No priority inheritance here, which the static mutex does have. Adding it
/// would mean this type reaching into the scheduler's boost bookkeeping, and
/// the blob's own locks are short and uncontended between its tasks. Worth
/// revisiting if a priority inversion is ever actually observed.
pub struct RecursiveMutex {
    /// Task id of the owner, or `NO_OWNER`.
    owner: u32,
    depth: u32,
}

/// Sentinel for "not held". Task ids are small indices, so `u32::MAX` is safe.
const NO_OWNER: u32 = u32::MAX;

impl RecursiveMutex {
    pub const fn new() -> Self {
        Self { owner: NO_OWNER, depth: 0 }
    }

    pub fn is_held(&self) -> bool {
        self.owner != NO_OWNER
    }

    pub fn depth(&self) -> u32 {
        self.depth
    }

    fn key(&self) -> usize {
        self as *const _ as usize
    }

    /// Lock without blocking. Succeeds if free, or if this task already holds
    /// it — in which case the depth goes up.
    pub fn try_lock(&mut self, me: u32) -> bool {
        if self.owner == NO_OWNER {
            self.owner = me;
            self.depth = 1;
            true
        } else if self.owner == me {
            self.depth += 1;
            true
        } else {
            false
        }
    }

    /// Lock, blocking up to `timeout_ms`.
    pub fn lock(&mut self, me: u32, timeout_ms: u32) -> bool {
        loop {
            if self.try_lock(me) {
                return true;
            }
            if !block_recv(self.key(), timeout_ms) {
                return false;
            }
        }
    }

    /// Release one level. `false` if the caller is not the owner, which is a
    /// bug worth reporting rather than ignoring.
    pub fn unlock(&mut self, me: u32) -> bool {
        if self.owner != me || self.depth == 0 {
            return false;
        }
        self.depth -= 1;
        if self.depth == 0 {
            self.owner = NO_OWNER;
            wake_one_receiver(self.key());
        }
        true
    }
}

impl Default for RecursiveMutex {
    fn default() -> Self {
        Self::new()
    }
}

/// Bits that tasks can wait on, any or all.
///
/// Twenty-four bits, matching what the blob expects; the top eight of a `u32`
/// are reserved and rejected rather than silently masked off.
pub struct EventGroup {
    bits: AtomicU32,
}

/// The usable bits. The blob's own API reserves the top byte for control.
pub const EVENT_BITS_MASK: u32 = 0x00FF_FFFF;

impl EventGroup {
    pub const fn new() -> Self {
        Self { bits: AtomicU32::new(0) }
    }

    pub fn get(&self) -> u32 {
        self.bits.load(Ordering::Acquire)
    }

    fn key(&self) -> usize {
        self as *const _ as usize
    }

    /// Set bits and wake everyone waiting, since any of them might now be
    /// satisfied and this type cannot tell which without checking each.
    ///
    /// Returns the value after setting. Bits outside [`EVENT_BITS_MASK`] are
    /// ignored.
    pub fn set(&self, bits: u32) -> u32 {
        let bits = bits & EVENT_BITS_MASK;
        let after = self.bits.fetch_or(bits, Ordering::AcqRel) | bits;
        // Wake all: a waiter whose condition is still unmet re-blocks. Waking
        // one would strand a task whose bits are set behind one whose are not.
        for _ in 0..MAX_WAKE {
            wake_one_receiver(self.key());
        }
        after
    }

    /// Clear bits. Returns the value before clearing.
    pub fn clear(&self, bits: u32) -> u32 {
        self.bits.fetch_and(!(bits & EVENT_BITS_MASK), Ordering::AcqRel)
    }

    /// Whether `bits` are satisfied by `current`, under `wait_for_all`.
    fn satisfied(current: u32, bits: u32, wait_for_all: bool) -> bool {
        if wait_for_all {
            current & bits == bits
        } else {
            current & bits != 0
        }
    }

    /// Wait for `bits`, any or all, up to `timeout_ms`.
    ///
    /// Returns the bits at the moment the wait was satisfied, or `None` on
    /// timeout. With `clear_on_exit`, the waited-for bits are cleared before
    /// returning — atomically with the test, so two waiters cannot both
    /// consume the same set.
    pub fn wait(
        &self,
        bits: u32,
        wait_for_all: bool,
        clear_on_exit: bool,
        timeout_ms: u32,
    ) -> Option<u32> {
        let bits = bits & EVENT_BITS_MASK;
        if bits == 0 {
            return None;
        }
        loop {
            let taken = crate::arch::cs_with(|| {
                let current = self.bits.load(Ordering::Acquire);
                if Self::satisfied(current, bits, wait_for_all) {
                    if clear_on_exit {
                        self.bits.fetch_and(!bits, Ordering::AcqRel);
                    }
                    Some(current)
                } else {
                    None
                }
            });
            if let Some(v) = taken {
                return Some(v);
            }
            if !block_recv(self.key(), timeout_ms) {
                return None;
            }
        }
    }
}

impl Default for EventGroup {
    fn default() -> Self {
        Self::new()
    }
}

/// How many waiters `EventGroup::set` will try to wake.
///
/// `wake_one_receiver` is a no-op once the list is empty, so this is an upper
/// bound rather than a count. It matches the scheduler's task capacity, since
/// no more tasks than that can be waiting.
const MAX_WAKE: usize = 16;

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The heap has to exist before anything here can be created. Idempotent,
    /// so every test can call it.
    fn heap_ready() {
        unsafe { heap::init(0) };
    }

    #[test]
    fn a_queue_round_trips_bytes_of_any_size() {
        heap_ready();
        for item_size in [1usize, 3, 4, 7, 16] {
            let mut q = DynQueue::create(4, item_size).expect("create");
            assert_eq!(q.item_size(), item_size);
            let src: [u8; 16] = core::array::from_fn(|i| (i as u8) ^ 0xA5);
            assert!(unsafe { q.try_send(src.as_ptr()) });
            assert_eq!(q.len(), 1);
            let mut out = [0u8; 16];
            assert!(unsafe { q.try_recv(out.as_mut_ptr()) });
            assert_eq!(&out[..item_size], &src[..item_size], "item_size {item_size}");
            assert!(q.is_empty());
            q.delete();
        }
    }

    #[test]
    fn a_queue_is_first_in_first_out_and_wraps() {
        heap_ready();
        let mut q = DynQueue::create(3, 1).expect("create");
        // Fill, drain, refill: the second pass crosses the ring boundary.
        for round in 0..3u8 {
            for i in 0..3u8 {
                let v = round * 10 + i;
                assert!(unsafe { q.try_send(&v) });
            }
            assert!(q.is_full());
            assert!(!unsafe { q.try_send(&0) }, "a full queue must refuse");
            for i in 0..3u8 {
                let mut got = 0u8;
                assert!(unsafe { q.try_recv(&mut got) });
                assert_eq!(got, round * 10 + i, "order broken on round {round}");
            }
            assert!(q.is_empty());
            assert!(!unsafe { q.try_recv(&mut 0u8) }, "an empty queue must refuse");
        }
        q.delete();
    }

    #[test]
    fn a_queue_with_a_zero_dimension_is_refused() {
        heap_ready();
        assert!(DynQueue::create(0, 4).is_none());
        assert!(DynQueue::create(4, 0).is_none());
        // And an item count that would overflow the multiply.
        assert!(DynQueue::create(usize::MAX, 2).is_none());
    }

    #[test]
    fn queues_do_not_share_storage() {
        heap_ready();
        let mut a = DynQueue::create(2, 4).expect("create");
        let mut b = DynQueue::create(2, 4).expect("create");
        let av = 0xAAAA_AAAAu32;
        let bv = 0xBBBB_BBBBu32;
        assert!(unsafe { a.try_send(&av as *const u32 as *const u8) });
        assert!(unsafe { b.try_send(&bv as *const u32 as *const u8) });
        let mut got = 0u32;
        assert!(unsafe { a.try_recv(&mut got as *mut u32 as *mut u8) });
        assert_eq!(got, av, "queue a returned queue b's item");
        assert!(unsafe { b.try_recv(&mut got as *mut u32 as *mut u8) });
        assert_eq!(got, bv);
        a.delete();
        b.delete();
    }

    #[test]
    fn deleting_a_queue_returns_its_storage() {
        heap_ready();
        let before = heap::free_bytes(Caps::Internal);
        let q = DynQueue::create(64, 16).expect("create");
        assert!(heap::free_bytes(Caps::Internal) < before);
        q.delete();
        assert_eq!(
            heap::free_bytes(Caps::Internal),
            before,
            "deleting a queue must return every byte"
        );
    }

    #[test]
    fn deleting_a_queue_with_items_still_in_it_is_fine() {
        // Items are bytes; there is nothing to drop. This is why `delete`
        // does not need to drain first.
        heap_ready();
        let before = heap::free_bytes(Caps::Internal);
        let mut q = DynQueue::create(8, 4).expect("create");
        for i in 0..8u32 {
            assert!(unsafe { q.try_send(&i as *const u32 as *const u8) });
        }
        q.delete();
        assert_eq!(heap::free_bytes(Caps::Internal), before);
    }

    #[test]
    fn a_semaphore_counts_up_to_its_maximum() {
        let mut s = Semaphore::create(3, 0).expect("create");
        assert_eq!(s.count(), 0);
        assert!(!s.try_take(), "an empty semaphore must not hand out a permit");
        for expect in 1..=3 {
            assert!(s.give());
            assert_eq!(s.count(), expect);
        }
        assert!(!s.give(), "giving past the maximum must be refused, not saturate");
        assert_eq!(s.count(), 3);
        for _ in 0..3 {
            assert!(s.try_take());
        }
        assert!(!s.try_take());
    }

    #[test]
    fn a_binary_semaphore_starts_empty_and_holds_one() {
        let mut s = Semaphore::binary();
        assert_eq!(s.count(), 0);
        assert!(s.give());
        assert!(!s.give(), "a binary semaphore must not exceed one");
        assert!(s.try_take());
        assert!(!s.try_take());
    }

    #[test]
    fn a_semaphore_with_impossible_bounds_is_refused() {
        assert!(Semaphore::create(0, 0).is_none(), "a maximum of zero is meaningless");
        assert!(Semaphore::create(2, 3).is_none(), "initial above maximum");
        assert!(Semaphore::create(2, 2).is_some(), "initial equal to maximum is fine");
    }

    #[test]
    fn a_recursive_mutex_lets_the_owner_back_in() {
        let mut m = RecursiveMutex::new();
        const ME: u32 = 1;
        assert!(!m.is_held());
        assert!(m.try_lock(ME));
        assert_eq!(m.depth(), 1);
        // The re-entry the static mutex refuses, and the whole reason this
        // type exists.
        assert!(m.try_lock(ME));
        assert!(m.try_lock(ME));
        assert_eq!(m.depth(), 3);
        assert!(m.unlock(ME));
        assert!(m.unlock(ME));
        assert!(m.is_held(), "still held until the last unlock");
        assert!(m.unlock(ME));
        assert!(!m.is_held());
    }

    #[test]
    fn a_recursive_mutex_still_excludes_other_tasks() {
        let mut m = RecursiveMutex::new();
        assert!(m.try_lock(1));
        assert!(!m.try_lock(2), "recursion is for the owner, not for everyone");
        assert!(m.unlock(1));
        assert!(m.try_lock(2), "free once the owner is done");
    }

    #[test]
    fn only_the_owner_can_unlock() {
        let mut m = RecursiveMutex::new();
        assert!(m.try_lock(1));
        assert!(!m.unlock(2), "a non-owner unlocking must be refused");
        assert_eq!(m.depth(), 1, "and must not have changed the depth");
        assert!(m.unlock(1));
        assert!(!m.unlock(1), "unlocking an unheld mutex must be refused");
    }

    #[test]
    fn event_bits_set_and_clear() {
        let g = EventGroup::new();
        assert_eq!(g.get(), 0);
        g.set(0b1010);
        assert_eq!(g.get(), 0b1010);
        g.set(0b0101);
        assert_eq!(g.get(), 0b1111);
        let before = g.clear(0b0011);
        assert_eq!(before, 0b1111, "clear reports the value before clearing");
        assert_eq!(g.get(), 0b1100);
    }

    #[test]
    fn event_bits_above_the_mask_are_ignored() {
        // The top byte is the blob's control space. Silently masking would let
        // a caller believe a bit was set that never was.
        let g = EventGroup::new();
        g.set(0xFF00_0000 | 0x01);
        assert_eq!(g.get(), 0x01);
        assert_eq!(EVENT_BITS_MASK, 0x00FF_FFFF);
    }

    #[test]
    fn waiting_for_any_and_all_differ_where_it_matters() {
        assert!(EventGroup::satisfied(0b0001, 0b0011, false), "any: one bit is enough");
        assert!(!EventGroup::satisfied(0b0001, 0b0011, true), "all: one bit is not");
        assert!(EventGroup::satisfied(0b0011, 0b0011, true), "all: both present");
        assert!(!EventGroup::satisfied(0b0000, 0b0011, false), "any: none present");
        // Unrelated bits set must not satisfy a wait.
        assert!(!EventGroup::satisfied(0b1100, 0b0011, false));
    }

    #[test]
    fn a_wait_already_satisfied_returns_without_blocking() {
        let g = EventGroup::new();
        g.set(0b0110);
        // Zero timeout: if this blocked at all it would return None.
        assert_eq!(g.wait(0b0010, false, false, 0), Some(0b0110));
        assert_eq!(g.get(), 0b0110, "no clear_on_exit means the bits stay");
    }

    #[test]
    fn clear_on_exit_consumes_only_the_waited_for_bits() {
        let g = EventGroup::new();
        g.set(0b1111);
        assert_eq!(g.wait(0b0011, true, true, 0), Some(0b1111));
        assert_eq!(g.get(), 0b1100, "only the waited-for bits are consumed");
    }

    #[test]
    fn waiting_for_no_bits_is_refused() {
        let g = EventGroup::new();
        assert_eq!(g.wait(0, false, false, 0), None);
        assert_eq!(g.wait(0xFF00_0000, false, false, 0), None, "reserved bits alone are nothing");
    }
}

// ── Task lifecycle and spinlock handles ─────────────────────────────────────

/// An opaque spinlock, created and destroyed at runtime.
///
/// Wraps [`crate::smp::Spinlock`] so the blob gets a handle it can pass around.
/// The wrapped type's interrupt-masking order is load-bearing and is preserved
/// exactly: it masks on the calling core for the whole critical section. The
/// other order — take the lock, then mask — deadlocks when a handler on the
/// same core wants the lock, which is precisely the case the radio hits.
///
/// Reentrancy still panics rather than hanging, as it does for every other
/// user of `Spinlock`. That is deliberate: a hang reports nothing.
pub struct SpinlockHandle {
    inner: crate::smp::Spinlock<()>,
}

impl SpinlockHandle {
    pub const fn new() -> Self {
        Self { inner: crate::smp::Spinlock::new(()) }
    }

    /// Run `f` holding the lock, with interrupts masked on this core.
    pub fn with<R>(&self, f: impl FnOnce() -> R) -> R {
        self.inner.with(|()| f())
    }
}

impl Default for SpinlockHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// The lowest-numbered — that is, highest — priority a task may be given.
///
/// FlintOS numbers priorities so that smaller is more urgent. The blob assumes
/// the opposite in places, so the adapter has to invert; this constant and
/// [`LOWEST_PRIORITY`] are what it inverts between.
pub const HIGHEST_PRIORITY: u8 = 0;

/// The highest-numbered — least urgent — priority. `Background(15)`.
pub const LOWEST_PRIORITY: u8 = crate::scheduler::MAX_PUBLIC_PRIORITY;

/// The id of the task that is running now.
pub fn current_task() -> u32 {
    crate::scheduler::with(|s| s.current())
}

/// Ask for a reschedule from an interrupt handler.
///
/// Does not switch here — the switch happens on the way out of the handler,
/// which is the only point at which it is safe.
pub fn yield_from_isr() {
    crate::scheduler::set_pending_switch();
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[test]
    fn the_priority_bounds_match_the_scheduler() {
        // The adapter inverts between these and the blob's ordering, so a
        // mismatch here shows up as a radio task at the wrong urgency —
        // which looks like a timing bug, not a constant.
        assert_eq!(HIGHEST_PRIORITY, 0);
        assert_eq!(LOWEST_PRIORITY, crate::scheduler::MAX_PUBLIC_PRIORITY);
        assert!(HIGHEST_PRIORITY < LOWEST_PRIORITY, "smaller must mean more urgent");
        assert_eq!(hal::types::Priority::Background(15).numeric(), LOWEST_PRIORITY);
    }

    #[test]
    fn a_spinlock_handle_runs_its_critical_section() {
        let lock = SpinlockHandle::new();
        let mut counter = 0u32;
        lock.with(|| counter += 1);
        lock.with(|| counter += 1);
        assert_eq!(counter, 2);
        // Sequential use must not wedge: the lock has to be released on the
        // way out, not merely on drop of something the caller holds.
        assert_eq!(lock.with(|| 42), 42);
    }
}
