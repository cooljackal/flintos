// SPDX-License-Identifier: Apache-2.0

//! Bounded typed message queue.
//!
//! `Queue<T, N>` is a lock-free bounded ring safe for **multiple producers and
//! a single consumer** (the common ISR-producer + task-consumer pattern) and
//! from interrupt context (plan W2.3) — no external locking required. Each
//! slot carries its own `Free -> Writing -> Ready -> Reading -> Free` state
//! (item 4): a producer must win a per-slot CAS out of `Free` before it may
//! also claim the `tail` position that maps to that slot, and a consumer must
//! win a per-slot CAS out of `Ready` before it may claim `head`. This closes
//! a claim-before-read race the old design had: with only a `tail - head < N`
//! count check and a boolean "ready" flag, a consumer that had claimed `head`
//! but not yet copied the payload out left the physical slot looking
//! available (by count) to a producer — notably `send_isr`, which really can
//! run in the window between a consumer's `head` CAS and its read — so the
//! producer could overwrite a message that had already been claimed but not
//! yet read. Gating on slot state instead of just the position counters means
//! a producer that can't win the slot's CAS fails fast (`Err`) rather than
//! spinning: correctness never depends on the reader (which might be the very
//! task an ISR producer just interrupted) making progress first, so
//! `send_isr` stays genuinely non-blocking.
//!
//! # Example (non-blocking)
//!
//! ```ignore
//! use api::queue::Queue;
//! static Q: Queue<u32, 16> = Queue::new();
//! assert!(Q.try_send(42).is_ok());
//! assert_eq!(Q.try_recv(), Ok(42));
//! ```

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use portable_atomic::{AtomicU8, AtomicUsize, Ordering};

/// Per-slot state machine (item 4). `Free` and `Ready` are the only states a
/// slot may be in when nobody holds it mid-transfer; `Writing`/`Reading` mark
/// exclusive in-progress access so the other side can never observe (let
/// alone overwrite) a half-written or half-read payload.
const SLOT_FREE: u8 = 0;
const SLOT_WRITING: u8 = 1;
const SLOT_READY: u8 = 2;
const SLOT_READING: u8 = 3;

/// A bounded, typed, lock-free queue (MPSC-safe).
pub struct Queue<T, const N: usize> {
    slots: [UnsafeCell<MaybeUninit<T>>; N],
    /// Per-slot state: Free -> Writing -> Ready -> Reading -> Free.
    state: [AtomicU8; N],
    /// Next position to write (producers reserve via CAS).
    tail: AtomicUsize,
    /// Next position to read (consumer claims via CAS).
    head: AtomicUsize,
}

unsafe impl<T: Send, const N: usize> Send for Queue<T, N> {}
unsafe impl<T: Send, const N: usize> Sync for Queue<T, N> {}

impl<T, const N: usize> Default for Queue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Queue<T, N> {
    /// Create a new, empty queue. `N` must be >= 1.
    pub const fn new() -> Self {
        Self {
            slots: [const { UnsafeCell::new(MaybeUninit::uninit()) }; N],
            state: [const { AtomicU8::new(SLOT_FREE) }; N],
            tail: AtomicUsize::new(0),
            head: AtomicUsize::new(0),
        }
    }

    /// Try to send a message without blocking. `Err(msg)` if the queue is
    /// full — which now also covers the case where the `tail - head` count
    /// says there's room but the physical slot hasn't actually been vacated
    /// yet (a consumer has claimed it but not finished reading, item 4). We
    /// never spin waiting for that to resolve: `send_isr` must stay
    /// non-blocking even when it's the ISR that interrupted the reader.
    pub fn try_send(&self, msg: T) -> Result<(), T> {
        // Bounds the "won the slot, lost the position CAS" retry below.
        // That branch is provably unreachable under this queue's actual
        // concurrency model (see the comment at the `store` below), but we
        // still bound it structurally rather than resting `send_isr`'s
        // non-blocking guarantee on that reasoning alone.
        let mut retries_left = N + 1;
        loop {
            let tail = self.tail.load(Ordering::Acquire);
            let head = self.head.load(Ordering::Acquire);
            if tail.wrapping_sub(head) >= N {
                return Err(msg); // full by count
            }
            let slot = tail % N;
            // Claim the physical slot *before* the logical position. A
            // single CAS attempt, never a retry loop: if this fails because
            // the slot is still `Reading` (or, extremely rarely, `Writing`/
            // `Ready` from another producer that raced us for the same
            // still-uncommitted `tail`), the safe and non-blocking answer is
            // "not available right now", not "spin until it is".
            if self.state[slot]
                .compare_exchange(SLOT_FREE, SLOT_WRITING, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(msg);
            }
            if self
                .tail
                .compare_exchange_weak(tail, tail + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                unsafe { (*self.slots[slot].get()).as_mut_ptr().write(msg) };
                self.state[slot].store(SLOT_READY, Ordering::Release);
                return Ok(());
            }
            // We won the slot but lost the `tail` CAS. Provably unreachable
            // under this queue's actual concurrency model — nobody else can
            // target the same slot while we hold it `Writing`, so nobody
            // else can be racing to move `tail` off the value we just read —
            // but we don't bet correctness on that reasoning holding forever
            // under future changes: release the slot and retry, bounded, so
            // `send_isr` structurally cannot spin unboundedly even if that
            // reasoning is ever invalidated.
            self.state[slot].store(SLOT_FREE, Ordering::Release);
            retries_left -= 1;
            if retries_left == 0 {
                return Err(msg);
            }
        }
    }

    /// Try to receive a message without blocking.
    pub fn try_recv(&self) -> Result<T, RecvError> {
        // See the matching comment in `try_send`.
        let mut retries_left = N + 1;
        loop {
            let head = self.head.load(Ordering::Acquire);
            let tail = self.tail.load(Ordering::Acquire);
            if head == tail {
                return Err(RecvError::Empty);
            }
            let slot = head % N;
            // Claim the physical slot *before* the logical position (mirrors
            // `try_send`): once this succeeds, a producer's own slot CAS in
            // `try_send` cannot succeed until we release the slot below,
            // which only happens after we've copied the payload out. That's
            // the fix for item 4 — the old code claimed `head` first and only
            // *then* read the payload, leaving a window where a producer
            // gated purely on the `tail - head` count could reuse the slot.
            if self.state[slot]
                .compare_exchange(SLOT_READY, SLOT_READING, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                // Producer reserved the position but hasn't published yet
                // (Writing), or another consumer already claimed it.
                return Err(RecvError::Contended);
            }
            if self
                .head
                .compare_exchange_weak(head, head + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let msg = unsafe { (*self.slots[slot].get()).as_ptr().read() };
                self.state[slot].store(SLOT_FREE, Ordering::Release);
                return Ok(msg);
            }
            // Won the slot, lost the `head` CAS to a racing consumer —
            // release and retry, bounded (see the symmetric note in
            // `try_send`; only reachable at all under genuine multi-consumer
            // use, which this type advertises as merely "robust under", not
            // the common case).
            self.state[slot].store(SLOT_READY, Ordering::Release);
            retries_left -= 1;
            if retries_left == 0 {
                return Err(RecvError::Contended);
            }
        }
    }

    /// Send from interrupt context — never blocks (single CAS attempts only
    /// on both the slot state and `tail`; see `try_send`).
    ///
    /// Wakes a blocked receiver on success, which is the entire point of the
    /// ISR-to-driver-task pattern this exists for. It used to be a bare
    /// `try_send`: the message landed in the ring and nothing woke the task
    /// waiting for it, so a driver task blocked in `recv(&Q, u32::MAX)` stayed
    /// blocked forever with its data sitting in the queue.
    pub fn send_isr(&self, msg: T) -> Result<(), T> {
        self.try_send(msg)?;
        wake_receiver(self.addr());
        Ok(())
    }

    /// Approximate number of messages currently queued.
    pub fn depth(&self) -> u32 {
        let t = self.tail.load(Ordering::Relaxed);
        let h = self.head.load(Ordering::Relaxed);
        t.wrapping_sub(h) as u32
    }

    pub const fn capacity(&self) -> u32 {
        N as u32
    }

    fn addr(&self) -> usize {
        self as *const _ as usize
    }
}

// ── Blocking send/recv (kernel waiter, plan W4.1) ──────────────────────────

/// Absolute wake deadline, computed once at entry to `send`/`recv` so retry
/// loops pass the *remaining* time to the kernel instead of re-arming the
/// full timeout on every retry (item 9). The kernel's `block_send`/
/// `block_recv` compute a fresh `now + timeout_ms` deadline on every call
/// (see `kernel/src/queue.rs::deadline_for`), so a retry loop that keeps
/// passing the original `timeout_ms` re-arms the full wait each time —
/// under enough contention (spurious wakeups, lost races) the *total* wait
/// is unbounded even though the caller asked for a bounded one. Pure
/// arithmetic, no I/O, so it's host-testable on its own (see the tests
/// below) independent of the kernel timeout plumbing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Deadline {
    /// `None` means "wait forever" (`timeout_ms == u32::MAX`), mirroring the
    /// kernel's own "0 = forever" sentinel one layer down.
    at: Option<u64>,
}

impl Deadline {
    /// Compute the absolute deadline for `timeout_ms`, anchored at `now`
    /// (milliseconds, same clock as `api::timer::now_ms`).
    fn new(timeout_ms: u32, now: u64) -> Self {
        if timeout_ms == u32::MAX {
            Deadline { at: None }
        } else {
            Deadline { at: Some(now.saturating_add(timeout_ms as u64)) }
        }
    }

    /// Milliseconds remaining at `now`, clamped to `u32` for the kernel call.
    /// `None` means the deadline has already passed — the caller should
    /// report a timeout without asking the kernel to block again.
    fn remaining_ms(&self, now: u64) -> Option<u32> {
        match self.at {
            None => Some(u32::MAX),
            Some(at) => {
                if now >= at {
                    None
                } else {
                    Some((at - now).min(u32::MAX as u64) as u32)
                }
            }
        }
    }
}

/// Send with timeout. `timeout_ms == 0` is non-blocking; `u32::MAX` waits
/// forever. Blocks via the kernel waiter list when the queue is full.
pub fn send<T, const N: usize>(
    q: &Queue<T, N>,
    msg: T,
    timeout_ms: u32,
) -> Result<(), SendError<T>> {
    #[cfg_attr(test, allow(unused_mut))] // only reassigned in the #[cfg(not(test))] retry loop below
    let mut msg = match q.try_send(msg) {
        Ok(()) => {
            wake_receiver(q.addr());
            return Ok(());
        }
        Err(m) => m,
    };
    if timeout_ms == 0 {
        return Err(SendError(msg));
    }
    #[cfg(not(test))]
    {
        let deadline = Deadline::new(timeout_ms, crate::timer::now_ms());
        loop {
            let remaining = match deadline.remaining_ms(crate::timer::now_ms()) {
                Some(r) => r,
                None => return Err(SendError(msg)), // deadline already passed
            };
            if !block_send(q.addr(), remaining) {
                return Err(SendError(msg)); // timed out
            }
            match q.try_send(msg) {
                Ok(()) => {
                    wake_receiver(q.addr());
                    return Ok(());
                }
                Err(m) => msg = m, // spurious / lost race — retry
            }
        }
    }
    #[cfg(test)]
    Err(SendError(msg))
}

/// Receive with timeout. `timeout_ms == 0` is non-blocking; `u32::MAX` waits
/// forever. Blocks via the kernel waiter list when the queue is empty.
pub fn recv<T, const N: usize>(q: &Queue<T, N>, timeout_ms: u32) -> Result<T, RecvError> {
    if let Ok(m) = q.try_recv() {
        wake_sender(q.addr());
        return Ok(m);
    }
    if timeout_ms == 0 {
        return Err(RecvError::Timeout);
    }
    #[cfg(not(test))]
    {
        let deadline = Deadline::new(timeout_ms, crate::timer::now_ms());
        loop {
            let remaining = match deadline.remaining_ms(crate::timer::now_ms()) {
                Some(r) => r,
                None => return Err(RecvError::Timeout), // deadline already passed
            };
            if !block_recv(q.addr(), remaining) {
                return Err(RecvError::Timeout); // timed out
            }
            if let Ok(m) = q.try_recv() {
                wake_sender(q.addr());
                return Ok(m);
            }
        }
    }
    #[cfg(test)]
    Err(RecvError::Timeout)
}

/// Current depth (free function mirror used by demos).
pub fn depth<T, const N: usize>(q: &Queue<T, N>) -> u32 {
    q.depth()
}

// Kernel waiter bridge.
#[cfg(not(test))]
fn block_send(q_addr: usize, timeout_ms: u32) -> bool {
    extern "Rust" {
        fn _flint_sys_queue_block_send(q_addr: usize, timeout_ms: u32) -> bool;
    }
    unsafe { _flint_sys_queue_block_send(q_addr, timeout_ms) }
}
#[cfg(not(test))]
fn block_recv(q_addr: usize, timeout_ms: u32) -> bool {
    extern "Rust" {
        fn _flint_sys_queue_block_recv(q_addr: usize, timeout_ms: u32) -> bool;
    }
    unsafe { _flint_sys_queue_block_recv(q_addr, timeout_ms) }
}
fn wake_receiver(q_addr: usize) {
    #[cfg(not(test))]
    unsafe {
        extern "Rust" {
            fn _flint_sys_queue_wake_receiver(q_addr: usize);
        }
        _flint_sys_queue_wake_receiver(q_addr);
    }
    #[cfg(test)]
    let _ = q_addr;
}
fn wake_sender(q_addr: usize) {
    #[cfg(not(test))]
    unsafe {
        extern "Rust" {
            fn _flint_sys_queue_wake_sender(q_addr: usize);
        }
        _flint_sys_queue_wake_sender(q_addr);
    }
    #[cfg(test)]
    let _ = q_addr;
}

/// Error returned by [`send`] when the message could not be delivered.
pub struct SendError<T>(pub T);

/// Why a receive failed.
///
/// One type for both the blocking and non-blocking paths. `try_recv` used to
/// return `Result<T, ()>`, which said only "not now" and left every caller to
/// guess which case it was in -- and they want opposite responses: `Empty`
/// means wait for a producer, `Contended` means retry immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvError {
    /// Nothing queued.
    Empty,
    /// A message exists but is not readable yet: a producer reserved the slot
    /// and has not published it, or another consumer claimed it first.
    /// Retrying straight away is the right response.
    Contended,
    /// [`recv`] waited and gave up.
    Timeout,
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn send_recv_roundtrip() {
        let q = Queue::<u32, 4>::new();
        assert!(q.try_send(10).is_ok());
        assert!(q.try_send(20).is_ok());
        assert_eq!(q.try_recv(), Ok(10));
        assert_eq!(q.try_recv(), Ok(20));
    }

    #[test]
    fn full_queue_rejects() {
        let q = Queue::<u32, 2>::new();
        assert!(q.try_send(1).is_ok());
        assert!(q.try_send(2).is_ok());
        assert_eq!(q.try_send(3), Err(3));
    }

    #[test]
    fn empty_queue_recv_fails() {
        let q = Queue::<u32, 4>::new();
        assert_eq!(q.try_recv(), Err(RecvError::Empty));
    }

    #[test]
    fn capacity_depth() {
        let q = Queue::<u32, 16>::new();
        assert_eq!(q.capacity(), 16);
        assert_eq!(q.depth(), 0);
        q.try_send(42).ok();
        assert_eq!(q.depth(), 1);
        q.try_recv().ok();
        assert_eq!(q.depth(), 0);
    }

    #[test]
    fn send_isr_delegates() {
        let q = Queue::<u32, 4>::new();
        assert!(q.send_isr(7).is_ok());
        assert_eq!(q.try_recv(), Ok(7));
    }

    #[test]
    fn blocking_send_fallback() {
        let q = Queue::<u32, 1>::new();
        assert!(send(&q, 1, 0).is_ok());
        assert!(send(&q, 2, 100).is_err());
    }

    #[test]
    fn blocking_recv_fallback() {
        let q = Queue::<u32, 4>::new();
        assert_eq!(recv(&q, 0), Err(RecvError::Timeout));
        assert_eq!(recv(&q, 100), Err(RecvError::Timeout));
        q.try_send(99).ok();
        assert_eq!(recv(&q, 0), Ok(99));
    }

    #[test]
    fn wrap_around() {
        let q = Queue::<u32, 3>::new();
        for i in 0..5 {
            q.try_send(i).ok();
            assert_eq!(q.try_recv(), Ok(i));
        }
    }

    #[test]
    fn recv_error_distinguishes_its_cases() {
        // The point of the enum: an empty queue and a contended slot are not
        // the same event and do not want the same response.
        assert_eq!(RecvError::Empty, RecvError::Empty);
        assert_ne!(RecvError::Empty, RecvError::Contended);
        assert_ne!(RecvError::Empty, RecvError::Timeout);
    }

    // ── Item 4: claim-before-read race ──────────────────────────────────────

    #[test]
    fn producer_cannot_reuse_slot_still_being_read() {
        // Single-slot queue so the very next `tail` position is guaranteed to
        // map to the slot a "consumer" is mid-read on — the exact collision
        // item 4 describes (a producer, e.g. `send_isr`, reusing a slot
        // between the consumer's `head` CAS and its payload read).
        let q = Queue::<u32, 1>::new();
        assert!(q.try_send(1).is_ok());

        // Simulate a consumer that has claimed the slot (advanced `head`,
        // moved the slot to `Reading`) but not yet finished copying the
        // payload out — the exact window the old design left unguarded.
        q.head.store(1, Ordering::Relaxed);
        q.state[0].store(SLOT_READING, Ordering::Relaxed);

        // Count-wise there's "room" (tail - head == 0 < 1), but the physical
        // slot is still live. A producer — including `send_isr`, which must
        // never block — must fail fast instead of overwriting it.
        assert_eq!(q.try_send(2), Err(2));
        assert_eq!(q.send_isr(3), Err(3));

        // Once the "consumer" finishes (releases the slot), sending works.
        q.state[0].store(SLOT_FREE, Ordering::Relaxed);
        assert!(q.try_send(4).is_ok());
    }

    #[test]
    fn consumer_cannot_claim_unpublished_slot() {
        // A producer that has reserved `tail` (Writing) but not yet
        // published (Ready) must not hand its half-written slot to a reader.
        //
        // Contended, not Empty: there *is* a message coming, so the caller
        // should retry rather than go and wait on a producer that has already
        // started. Distinguishing these two is what the enum is for -- under
        // `Result<T, ()>` this case was indistinguishable from an empty queue.
        let q = Queue::<u32, 1>::new();
        q.tail.store(1, Ordering::Relaxed);
        q.state[0].store(SLOT_WRITING, Ordering::Relaxed);
        assert_eq!(q.try_recv(), Err(RecvError::Contended));
    }

    // ── Item 9: deadline math ────────────────────────────────────────────────

    #[test]
    fn deadline_forever_never_expires() {
        let d = Deadline::new(u32::MAX, 1_000);
        assert_eq!(d.remaining_ms(1_000), Some(u32::MAX));
        assert_eq!(d.remaining_ms(u64::MAX), Some(u32::MAX));
    }

    #[test]
    fn deadline_counts_down_across_retries() {
        // This is the property item 9 fixes: the deadline is anchored once,
        // so the "remaining" time handed to each retry shrinks instead of
        // re-arming the full timeout every time.
        let d = Deadline::new(50, 100); // absolute deadline: tick 150
        assert_eq!(d.remaining_ms(100), Some(50));
        assert_eq!(d.remaining_ms(120), Some(30));
        assert_eq!(d.remaining_ms(149), Some(1));
    }

    #[test]
    fn deadline_expires_exactly_at_target() {
        let d = Deadline::new(50, 100);
        assert_eq!(d.remaining_ms(150), None); // now == at: expired
        assert_eq!(d.remaining_ms(200), None); // now  > at: expired
    }

    #[test]
    fn deadline_zero_timeout_is_immediately_due() {
        // `send`/`recv` special-case `timeout_ms == 0` before ever
        // constructing a `Deadline`, but the type itself should still behave
        // sensibly if asked: the deadline is "now", so it reads as already
        // expired one tick later, never as "forever".
        let d = Deadline::new(0, 1_000);
        assert_eq!(d.remaining_ms(1_001), None);
    }

    #[test]
    fn deadline_saturates_instead_of_overflowing() {
        // `now` near `u64::MAX` plus a finite timeout must saturate rather
        // than wrap around to a small/negative-looking deadline that expires
        // instantly despite a nonzero timeout being requested.
        let d = Deadline::new(1_000, u64::MAX - 10);
        // Saturated deadline is `u64::MAX`; 10 ms of the requested 1000 still
        // fit before hitting it.
        assert_eq!(d.remaining_ms(u64::MAX - 10), Some(10));
        assert_eq!(d.remaining_ms(u64::MAX), None);
    }
}

#[cfg(test)]
#[path = "queue_race_tests.rs"]
mod race_tests;
