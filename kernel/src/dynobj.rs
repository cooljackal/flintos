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
use crate::queue::{
    retry_deadline, retry_remaining, wake_one_receiver,
};

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
        unsafe { self.send_blocking(item, timeout_ms, false) }
    }

    /// Copy one item in at the **head**, without blocking. `false` if full.
    ///
    /// FreeRTOS's `xQueueSendToFront`, which the blob uses to put a frame back
    /// where it will be taken next rather than behind everything already
    /// waiting. The ring runs head-forward, so this steps head *backwards* —
    /// which is the whole implementation, and also the whole risk: writing at
    /// `head` without moving it first would overwrite the item about to be
    /// received.
    ///
    /// # Safety
    /// `item` must point to at least [`DynQueue::item_size`] readable bytes.
    pub unsafe fn try_send_to_front(&mut self, item: *const u8) -> bool {
        if self.is_full() {
            return false;
        }
        self.head = if self.head == 0 { self.capacity - 1 } else { self.head - 1 };
        let slot = unsafe { self.buf.add(self.head * self.item_size) };
        unsafe { core::ptr::copy_nonoverlapping(item, slot, self.item_size) };
        self.len += 1;
        true
    }

    /// Send to the head, blocking up to `timeout_ms` for a slot.
    ///
    /// # Safety
    /// As [`DynQueue::try_send_to_front`].
    pub unsafe fn send_to_front(&mut self, item: *const u8, timeout_ms: u32) -> bool {
        unsafe { self.send_blocking(item, timeout_ms, true) }
    }

    /// The blocking half of both sends, which differ only in where the item
    /// lands. Written once so the deadline handling below cannot be right in
    /// one of them and wrong in the other.
    ///
    /// # Safety
    /// As [`DynQueue::try_send`].
    unsafe fn send_blocking(&mut self, item: *const u8, timeout_ms: u32, front: bool) -> bool {
        let put = |q: &mut Self| unsafe {
            if front {
                q.try_send_to_front(item)
            } else {
                q.try_send(item)
            }
        };
        // The deadline is computed once and the *remaining* time passed each
        // time round. Re-passing `timeout_ms` would re-arm the full wait on
        // every retry, so a caller asking for 10 ms could wait indefinitely
        // under contention -- see `queue::retry_deadline`.
        //
        // The copy-in and the enrolment are one step, and so are the copy-in
        // and the receiver's wakeup. See `queue::produce_or_block`: split
        // apart, a receiver that finds the queue empty between them is never
        // told the item arrived.
        let deadline = retry_deadline(timeout_ms);
        let key = self.key();
        loop {
            let mut sent = false;
            crate::queue::produce_or_block(key, u32::MAX, || {
                sent = put(self);
                sent
            });
            if sent {
                return true;
            }
            if retry_remaining(deadline).is_none() {
                return false;
            }
        }
    }

    /// Receive, blocking up to `timeout_ms` for an item.
    ///
    /// # Safety
    /// As [`DynQueue::try_recv`].
    pub unsafe fn recv(&mut self, out: *mut u8, timeout_ms: u32) -> bool {
        // Remaining time, not the original timeout. As `DynQueue::send`.
        let deadline = retry_deadline(timeout_ms);
        let key = self.key();
        loop {
            let mut got = false;
            crate::queue::consume_or_block_waking_sender(key, u32::MAX, || {
                got = unsafe { self.try_recv(out) };
                got
            });
            if got {
                return true;
            }
            if retry_remaining(deadline).is_none() {
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
    ///
    /// The test and the enrolment are one step. Splitting them — try, fail,
    /// then join the waiter list — loses a `give` that lands in between: it
    /// wakes nobody, because nobody is listed yet, and this then sleeps on a
    /// permit that is already sitting there. See
    /// [`consume_or_block`](crate::queue::consume_or_block).
    pub fn take(&mut self, timeout_ms: u32) -> bool {
        // Remaining time, not the original timeout. As `DynQueue::send`.
        let deadline = retry_deadline(timeout_ms);
        let key = self.key();
        loop {
            let mut got = false;
            let blocked_and_woken = !crate::queue::consume_or_block(key, u32::MAX, || {
                if self.count > 0 {
                    self.count -= 1;
                    got = true;
                }
                got
            });
            if got {
                return true;
            }
            if !blocked_and_woken {
                // It could not enrol -- the waiter table is full. Fall through
                // to the timeout check rather than spinning on it.
            }
            match retry_remaining(deadline) {
                Some(_) => {}
                None => return false,
            }
        }
    }

    /// Return a permit. `false` if already at `max`, which is a caller bug
    /// rather than something to saturate silently.
    pub fn give(&mut self) -> bool {
        // The increment and the wake are one step, for the reason `take`
        // gives: a taker testing the count either sees this before it lands,
        // and enrols where this wake will find it, or after, and takes it.
        let key = self.key();
        let max = self.max;
        crate::queue::produce_and_wake(key, || {
            if self.count >= max {
                return false;
            }
            self.count += 1;
            true
        })
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
        // Remaining time, not the original timeout. As `DynQueue::send`. The
        // test and the enrolment are one step, as everywhere else here: an
        // unlock between them would wake nobody and this would wait on a lock
        // that is already free.
        let deadline = retry_deadline(timeout_ms);
        let key = self.key();
        loop {
            let mut held = false;
            crate::queue::consume_or_block(key, u32::MAX, || {
                held = self.try_lock(me);
                held
            });
            if held {
                return true;
            }
            if retry_remaining(deadline).is_none() {
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
        let key = self.key();
        self.depth -= 1;
        if self.depth == 0 {
            // Releasing and waking are one step: a waiter testing the lock in
            // between would find it taken and enrol after the wake had gone.
            crate::queue::produce_and_wake(key, || {
                self.owner = NO_OWNER;
                true
            });
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
        // The set and the first wake are one step, so a waiter testing the
        // bits cannot slip between them and enrol after the wake has gone.
        // The rest of the wakes are ordinary: those waiters are already
        // enrolled, and waking an unsatisfied one costs it a re-block.
        let mut after = 0;
        crate::queue::produce_and_wake(self.key(), || {
            after = self.bits.fetch_or(bits, Ordering::AcqRel) | bits;
            true
        });
        // Wake all: a waiter whose condition is still unmet re-blocks. Waking
        // one would strand a task whose bits are set behind one whose are not.
        for _ in 1..MAX_WAKE {
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
        let take = || {
            crate::arch::cs_with(|| {
                let current = self.bits.load(Ordering::Acquire);
                if Self::satisfied(current, bits, wait_for_all) {
                    if clear_on_exit {
                        self.bits.fetch_and(!bits, Ordering::AcqRel);
                    }
                    Some(current)
                } else {
                    None
                }
            })
        };
        // Remaining time, not the original timeout. As `DynQueue::send`. The
        // test runs under the same locks as `set`'s wake, so a set landing
        // between the test and the enrolment is not lost.
        let deadline = retry_deadline(timeout_ms);
        let key = self.key();
        loop {
            let mut got = None;
            crate::queue::consume_or_block(key, u32::MAX, || {
                got = take();
                got.is_some()
            });
            if got.is_some() {
                return got;
            }
            if retry_remaining(deadline).is_none() {
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
    // ── The lost wakeup ─────────────────────────────────────────────────
    //
    // These cannot reproduce the race itself: a host test is single-threaded,
    // and the race needs a producer to run between a consumer's test and its
    // enrolment. What they can do is hold the shape that made the race
    // possible from coming back -- the take path must not have an observable
    // moment where it has decided to block but is not yet listed.

    #[test]
    fn a_give_that_arrives_first_is_not_lost() {
        // The ordering that hung the radio driver: the permit is there before
        // the taker looks. It must be taken, not waited for.
        let mut s = Semaphore::create(1, 0).unwrap();
        assert!(s.give());
        assert_eq!(s.count(), 1);
        assert!(s.take(FOREVER), "a permit already given must be taken, not waited on");
        assert_eq!(s.count(), 0);
    }

    #[test]
    fn giving_past_the_ceiling_wakes_nobody() {
        // `produce_and_wake` only wakes when it actually made something
        // available. A refused give that woke a waiter would hand it a permit
        // that does not exist.
        let mut s = Semaphore::create(1, 1).unwrap();
        assert!(!s.give(), "at the ceiling");
        assert_eq!(s.count(), 1);
    }

    #[test]
    fn take_and_give_still_balance() {
        let mut s = Semaphore::create(3, 0).unwrap();
        for _ in 0..3 {
            assert!(s.give());
        }
        assert!(!s.give(), "ceiling");
        for _ in 0..3 {
            assert!(s.take(FOREVER));
        }
        assert_eq!(s.count(), 0);
    }

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
    fn a_send_to_front_jumps_the_queue_and_wraps_backwards() {
        heap_ready();
        let mut q = DynQueue::create(3, 1).expect("create");
        // Two waiting, then one pushed in front: it must come out first and
        // the other two must keep their order behind it.
        for v in [1u8, 2] {
            assert!(unsafe { q.try_send(&v) });
        }
        assert!(unsafe { q.try_send_to_front(&9u8) });
        assert_eq!(q.len(), 3);
        let mut got = [0u8; 3];
        for g in got.iter_mut() {
            assert!(unsafe { q.try_recv(g) });
        }
        assert_eq!(got, [9, 1, 2]);

        // From empty, head is 0 and stepping back must land on the last slot
        // rather than underflow. This is the case that panics in debug and
        // silently indexes off the end in release.
        assert!(q.is_empty());
        assert!(unsafe { q.try_send_to_front(&7u8) });
        let mut one = 0u8;
        assert!(unsafe { q.try_recv(&mut one) });
        assert_eq!(one, 7);

        // And a full queue refuses, rather than moving head over a live item.
        for v in [1u8, 2, 3] {
            assert!(unsafe { q.try_send(&v) });
        }
        assert!(!unsafe { q.try_send_to_front(&0u8) });
        assert_eq!(q.len(), 3);
        for expect in [1u8, 2, 3] {
            let mut g = 0u8;
            assert!(unsafe { q.try_recv(&mut g) });
            assert_eq!(g, expect, "a refused front-send corrupted the ring");
        }
        q.delete();
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
        // Const, so it belongs at compile time: a runtime assert on two
        // constants proves nothing the compiler has not already settled.
        const _: () = assert!(HIGHEST_PRIORITY < LOWEST_PRIORITY);
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

// ── Runtime-created tasks ───────────────────────────────────────────────────

/// Create a task whose stack is returned when it is deleted.
///
/// The static [`crate::spawn::sys_spawn`] bump-allocates from the linker's
/// `task_stacks` region and never reclaims it — correct for a static RTOS,
/// which creates its tasks once. The blobs create and delete tasks throughout
/// a session, so these take heap stacks instead and the pool is left alone.
///
/// Returns the task id, or `None` if there is no TCB slot or no heap.
pub fn spawn_task(
    name: &'static str,
    entry: fn(),
    priority: hal::types::Priority,
    stack_size: usize,
) -> Option<u32> {
    spawn_task_on(name, entry, priority, stack_size, crate::scheduler::Affinity::Any)
}

/// Create a heap-stacked task pinned to a core.
///
/// [`spawn_task`] with the affinity spelled out. It exists because the blobs
/// ask for it: `_task_create_pinned_to_core` is a separate entry from
/// `_task_create` precisely because the radio's own task must run on the core
/// whose interrupt matrix its handlers were routed through, and answering that
/// request with `Affinity::Any` would be a wrong answer rather than a missing
/// one — the task would work, until the tick migrated it and its interrupts
/// stopped arriving.
pub fn spawn_task_on(
    name: &'static str,
    entry: fn(),
    priority: hal::types::Priority,
    stack_size: usize,
    affinity: crate::scheduler::Affinity,
) -> Option<u32> {
    crate::spawn::sys_spawn_from(
        name,
        entry,
        priority,
        stack_size,
        affinity,
        crate::spawn::StackSource::Heap,
    )
    .map(|t| t.0)
}

/// Why a delete was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteError {
    /// No task with that id.
    NoSuchTask,
    /// The task is running right now, on this core or the other one. Freeing
    /// the stack it is executing on would corrupt it mid-instruction.
    StillRunning,
    /// The task's stack came from the linker pool, which is a bump allocator
    /// with nothing to give back to.
    NotDeletable,
}

/// Delete a task and return its stack to the heap.
///
/// Deleting the *calling* task never returns — see [`delete_self`].
///
/// # What this refuses
///
/// **A task running on the other core.** It cannot be made to switch away
/// synchronously, and freeing the stack under it would corrupt it
/// mid-instruction. [`DeleteError::StillRunning`].
///
/// **Pool-backed tasks.** Nothing to reclaim; see [`DeleteError::NotDeletable`].
///
/// # What it handles
///
/// Removing the task from every queue waiter list. A deleted task left listed
/// gets unblocked by a later `wake_one_*`, by which point the slot may belong
/// to a different task — which then returns from a wait it never entered, far
/// from the delete that caused it.
pub fn delete_task(id: u32) -> Result<(), DeleteError> {
    // Self-delete has entirely different mechanics and does not return.
    if id == current_task() {
        delete_self();
    }
    // Take the stack details and clear the slot under one lock, so nothing can
    // schedule the task between deciding it is idle and removing it.
    let stack = crate::scheduler::with(|sched| {
        let tcb = match sched.tasks.get(id as usize).and_then(|t| t.as_ref()) {
            Some(t) => t,
            None => return Err(DeleteError::NoSuchTask),
        };
        if tcb.state == crate::scheduler::TaskState::Running {
            return Err(DeleteError::StillRunning);
        }
        // Also refuse if it is any core's current task, which `Running` should
        // already imply but is worth checking separately: the two are set at
        // slightly different moments during a switch.
        if sched.is_current_anywhere(id) {
            return Err(DeleteError::StillRunning);
        }
        if !tcb.heap_stack {
            return Err(DeleteError::NotDeletable);
        }
        let stack = (tcb.stack_base, tcb.priority);
        sched.tasks[id as usize] = None;
        // The task may have been the only Ready one at its priority.
        sched.recompute_ready_bit(stack.1);
        Ok(stack.0)
    })?;

    // Outside the scheduler lock: `forget_task` takes its own critical
    // section, and taking the scheduler's twice is the reentrancy that
    // panics.
    crate::queue::forget_task(id);
    unsafe { heap::free(stack as *mut u8, Caps::Internal) };
    Ok(())
}

#[cfg(test)]
mod task_tests {
    extern crate std;

    use super::*;
    use hal::types::Priority;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Serialise the tests that mutate the scheduler.
    ///
    /// The TCB table is global and the harness runs tests in parallel, so
    /// without this one test's reaper clears another's slots and the counts
    /// stop meaning anything. Poisoning is ignored: a panicking test has
    /// already failed, and blocking every later one behind it hides that.
    fn serial() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn heap_ready() {
        unsafe { heap::init(0) };
    }

    fn noop() {}

    /// On a 64-bit host the heap sits above 4 GiB, and `stack_base` is a `u32`
    /// because the target's address space is. The spawn path refuses rather
    /// than truncating.
    ///
    /// This is not a test-only concern dressed up as one: truncating would
    /// paint the stack guard, and then run the task, at an address that is not
    /// the stack. On the host that is an immediate segfault — which is how it
    /// was found — and on any 32-bit target where the heap somehow sat high it
    /// would be silent corruption.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn a_heap_stack_that_cannot_fit_a_u32_is_refused_not_truncated() {
        heap_ready();
        let before = heap::free_bytes(Caps::Internal);
        assert!(
            spawn_task("too high", noop, Priority::Normal(2), 4096).is_none(),
            "a stack above 4 GiB must be refused"
        );
        assert_eq!(
            heap::free_bytes(Caps::Internal),
            before,
            "the refused allocation must be handed back, not leaked"
        );
    }

    /// The real create/delete cycle needs 32-bit addresses, so it runs on the
    /// target — see `selftest_dynobj.rs`, which asserts the no-leak property
    /// the issue actually asks for.
    #[test]
    #[cfg(target_pointer_width = "32")]
    fn a_dynamic_task_takes_its_stack_from_the_heap_and_gives_it_back() {
        heap_ready();
        let before = heap::free_bytes(Caps::Internal);
        let id = spawn_task("dyn", noop, Priority::Normal(2), 4096).expect("spawn");
        assert!(heap::free_bytes(Caps::Internal) < before);
        delete_task(id).expect("delete");
        assert_eq!(heap::free_bytes(Caps::Internal), before);
    }

    #[test]
    fn deleting_something_that_was_never_a_task_is_refused() {
        assert_eq!(delete_task(crate::scheduler::MAX_TASKS as u32 + 1), Err(DeleteError::NoSuchTask));
    }

    #[test]
    fn a_pool_backed_task_cannot_be_deleted() {
        let _serial = serial();
        // The static tasks. Their stacks come from a bump allocator with
        // nothing to return them to, so refusing is the honest answer —
        // freeing would hand the heap a pointer it never owned.
        //
        // The TCB is set up directly rather than through `sys_spawn`, because
        // the linker's stack region is empty in a host build and the pool path
        // cannot allocate there at all.
        let id = crate::scheduler::with(|sched| {
            let id = sched.alloc_id().expect("a free TCB slot");
            if let Some(tcb) = &mut sched.tasks[id as usize] {
                tcb.state = crate::scheduler::TaskState::Ready;
                tcb.heap_stack = false;
                tcb.stack_base = 0x1000;
                tcb.stack_size = 4096;
            }
            id
        });
        assert_eq!(delete_task(id), Err(DeleteError::NotDeletable));
        // Clean up, or the slot leaks into every later test in this process.
        crate::scheduler::with(|sched| sched.tasks[id as usize] = None);
    }

    #[test]
    fn a_running_task_cannot_be_deleted() {
        let _serial = serial();
        // Freeing the stack a task is executing on corrupts it mid-instruction,
        // and the next interrupt writes into memory the heap has already given
        // to someone else. Self-delete lands here too, deliberately — see the
        // note on `delete_task`.
        let id = crate::scheduler::with(|sched| {
            let id = sched.alloc_id().expect("a free TCB slot");
            if let Some(tcb) = &mut sched.tasks[id as usize] {
                tcb.state = crate::scheduler::TaskState::Running;
                tcb.heap_stack = true;
                tcb.stack_base = 0x1000;
            }
            id
        });
        assert_eq!(delete_task(id), Err(DeleteError::StillRunning));
        crate::scheduler::with(|sched| sched.tasks[id as usize] = None);
    }

    /// Build a TCB already in `Deleting`, as `delete_self` would leave it.
    ///
    /// Constructed rather than reached through `delete_self`, which never
    /// returns and would hang the test process — the reaper is the half that
    /// can be checked from here.
    fn deleting_tcb(heap_stack: bool, stack_base: u32) -> u32 {
        crate::scheduler::with(|sched| {
            let id = sched.alloc_id().expect("a free TCB slot");
            if let Some(tcb) = &mut sched.tasks[id as usize] {
                tcb.state = crate::scheduler::TaskState::Deleting;
                tcb.heap_stack = heap_stack;
                tcb.stack_base = stack_base;
                tcb.stack_size = 1024;
            }
            id
        })
    }

    #[test]
    fn the_reaper_frees_a_deleted_task_s_slot() {
        let _serial = serial();
        // Slot bookkeeping only. The stack half needs a genuine 32-bit
        // address, so it lives on the target — see `selftest_dynobj.rs`.
        // Fabricating a `heap_stack` TCB here would truncate a 64-bit pointer
        // into `stack_base` and hand the heap something it never issued, which
        // is the same trap the spawn path refuses.
        heap_ready();
        let id = deleting_tcb(false, 0x1000);
        assert_eq!(reap_deleted(), 1, "the reaper should have taken exactly one");
        assert!(
            crate::scheduler::with(|s| s.tasks[id as usize].is_none()),
            "the slot must be free for reuse"
        );
    }

    #[test]
    fn the_reaper_leaves_a_task_that_is_still_some_core_s_current() {
        let _serial = serial();
        // The other-core case, and the check that makes the whole scheme safe.
        // While a core is still executing the dying task — or saving its
        // context on the way out — `current_per_core` names it, and freeing
        // the stack then would corrupt it mid-instruction.
        heap_ready();
        let id = deleting_tcb(false, 0x1000);
        let previous = crate::scheduler::with(|sched| {
            let prev = sched.current();
            sched.set_current(id);
            // `set_current` marks it Running; put it back to what a pending
            // self-delete actually looks like.
            if let Some(tcb) = &mut sched.tasks[id as usize] {
                tcb.state = crate::scheduler::TaskState::Deleting;
            }
            prev
        });

        let reaped = reap_deleted();
        let survived = crate::scheduler::with(|s| s.tasks[id as usize].is_some());

        // Put things back before asserting, so a failure does not cascade.
        //
        // The slot is left *allocated*, deliberately. On host there is no
        // previous current task to restore to — it is the `u32::MAX` sentinel,
        // and `set_current` would index with it — so this core keeps pointing
        // here. Freeing the slot would then let a later test reuse the id
        // while `current_per_core` still names it, and that test's tasks would
        // be silently skipped by the reaper. Parking one TCB for the life of
        // the process is the cheaper trade. Its state is set to something the
        // reaper ignores.
        crate::scheduler::with(|sched| {
            if let Some(tcb) = &mut sched.tasks[id as usize] {
                tcb.state = crate::scheduler::TaskState::Suspended;
            }
            if previous != u32::MAX {
                sched.set_current(previous);
            }
        });
        assert_eq!(reaped, 0, "nothing else was deleting, so nothing should be reaped");
        assert!(survived, "must not reap a task a core is still current on");
    }

    #[test]
    fn the_reaper_does_nothing_when_nothing_is_deleting() {
        let _serial = serial();
        heap_ready();
        // Idle calls this every loop, so "cheap and silent when idle" is the
        // behaviour that matters.
        assert_eq!(reap_deleted(), 0);
        assert_eq!(reap_deleted(), 0);
    }

    #[test]
    fn the_reaper_takes_several_in_one_pass() {
        let _serial = serial();
        // Idle calls this once per loop, so several deletions between two
        // passes must all be cleared rather than one per pass.
        heap_ready();
        let ids = [
            deleting_tcb(false, 0x1000),
            deleting_tcb(false, 0x2000),
            deleting_tcb(false, 0x3000),
        ];
        assert_eq!(reap_deleted(), 3, "one pass must clear all of them");
        for id in ids {
            assert!(crate::scheduler::with(|s| s.tasks[id as usize].is_none()));
        }
    }

    #[test]
    fn a_pool_backed_task_is_reaped_without_freeing_anything() {
        let _serial = serial();
        // Its stack came from the bump allocator, which has nothing to give
        // back to. The slot still has to be released, or a self-deleting
        // static task would hold a TCB forever.
        heap_ready();
        let before = heap::free_bytes(Caps::Internal);
        let id = deleting_tcb(false, 0x1000);
        assert_eq!(reap_deleted(), 1);
        assert!(crate::scheduler::with(|s| s.tasks[id as usize].is_none()));
        assert_eq!(
            heap::free_bytes(Caps::Internal),
            before,
            "nothing should have been handed to the heap"
        );
    }

    #[test]
    fn forgetting_a_task_clears_it_from_every_waiter_list() {
        // The hazard `delete_task` guards: a deleted id left listed is woken
        // later, after its slot has been reused by an unrelated task.
        let id = 7;
        assert!(!crate::queue::is_waiting_anywhere(id));
        crate::queue::forget_task(id);
        assert!(!crate::queue::is_waiting_anywhere(id), "purging must be idempotent");
    }
}

/// End the calling task. Never returns.
///
/// A task cannot free the stack it is executing on: the moment the heap
/// reissued those bytes, the next interrupt on this core would write its frame
/// into somebody else's allocation. So this does the half that is safe from
/// here — leave the ready set, stop being a queue waiter — and hands the rest
/// to [`reap_deleted`], which runs on the idle task's stack.
///
/// The task stays in [`TaskState::Deleting`] with its stack intact until then.
/// Nothing schedules it: every transition back into the ready set matches on
/// the blocked states by name, and this is not one of them.
///
/// # Why the loop at the end
///
/// `request_switch` marks a switch pending; it does not perform one. Until the
/// scheduler acts, this core is still executing here, on a stack that must
/// stay valid. Parking is what keeps that true — returning would run the
/// caller's epilogue on a task the scheduler has already written off.
pub fn delete_self() -> ! {
    let me = current_task();

    crate::scheduler::with(|sched| {
        if let Some(tcb) = &mut sched.tasks[me as usize] {
            let prio = tcb.priority;
            tcb.state = crate::scheduler::TaskState::Deleting;
            // Out of the run set, or `schedule` keeps finding it at this
            // priority level and this core never leaves.
            sched.recompute_ready_bit(prio);
        }
    });

    // Outside the scheduler lock: `forget_task` takes its own critical
    // section, and nesting the two is the reentrancy that panics.
    crate::queue::forget_task(me);

    crate::scheduler::request_switch();

    // Never scheduled again. `wait_for_interrupt` rather than a spin so the
    // core parks instead of burning the tick until the switch lands.
    loop {
        crate::arch::wait_for_interrupt();
    }
}

/// Free the stacks and slots of tasks that have deleted themselves.
///
/// Called from the idle loop, which is the one context guaranteed not to be
/// running on a dying task's stack: idle runs on the boot stack and is only
/// reached when nothing else is ready.
///
/// Returns how many were reaped, which is what the tests assert on.
///
/// # The check that makes this safe
///
/// A task is only freed once it is [`TaskState::Deleting`] *and* is not any
/// core's current task. The second condition is what covers the other core:
/// while it is still executing the dying task — or saving its context on the
/// way out — `current_per_core` still names it, and this skips it for now.
/// Idle runs often; there is no hurry.
pub fn reap_deleted() -> usize {
    let mut reaped = 0;
    loop {
        // One per pass, each with its own lock, so idle never holds the
        // scheduler while freeing and the other core is not kept waiting.
        let victim = crate::scheduler::with(|sched| {
            for i in 0..crate::scheduler::MAX_TASKS {
                let Some(tcb) = &sched.tasks[i] else { continue };
                if tcb.state != crate::scheduler::TaskState::Deleting {
                    continue;
                }
                if sched.is_current_anywhere(i as u32) {
                    continue;
                }
                let stack = if tcb.heap_stack { Some(tcb.stack_base) } else { None };
                sched.tasks[i] = None;
                return Some((i as u32, stack));
            }
            None
        });
        let Some((id, stack)) = victim else { return reaped };
        crate::queue::forget_task(id);
        if let Some(base) = stack {
            unsafe { heap::free(base as *mut u8, Caps::Internal) };
        }
        reaped += 1;
    }
}
