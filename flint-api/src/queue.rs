// SPDX-License-Identifier: Apache-2.0

//! Bounded typed message queue.
//!
//! `Queue<T, N>` is a lock-free bounded ring safe for **multiple producers and
//! a single consumer** (the common ISR-producer + task-consumer pattern) and
//! from interrupt context (plan W2.3) — no external locking required. Producers
//! reserve a slot with a `tail` CAS and publish via a per-slot ready flag;
//! the consumer claims with a `head` CAS, so it is also robust under occasional
//! multi-consumer use. Blocking send/recv with timeout layer on top via the
//! kernel waiter syscalls (plan W4.1).
//!
//! # Example (non-blocking)
//!
//! ```ignore
//! use flint_api::queue::Queue;
//! static Q: Queue<u32, 16> = Queue::new();
//! assert!(Q.try_send(42).is_ok());
//! assert_eq!(Q.try_recv(), Ok(42));
//! ```

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// A bounded, typed, lock-free queue (MPSC-safe).
pub struct Queue<T, const N: usize> {
    slots: [UnsafeCell<MaybeUninit<T>>; N],
    /// Per-slot "value published" flags.
    ready: [AtomicBool; N],
    /// Next position to write (producers reserve via CAS).
    tail: AtomicUsize,
    /// Next position to read (consumer claims via CAS).
    head: AtomicUsize,
}

unsafe impl<T: Send, const N: usize> Send for Queue<T, N> {}
unsafe impl<T: Send, const N: usize> Sync for Queue<T, N> {}

impl<T, const N: usize> Queue<T, N> {
    /// Create a new, empty queue. `N` must be >= 1.
    pub const fn new() -> Self {
        Self {
            slots: [const { UnsafeCell::new(MaybeUninit::uninit()) }; N],
            ready: [const { AtomicBool::new(false) }; N],
            tail: AtomicUsize::new(0),
            head: AtomicUsize::new(0),
        }
    }

    /// Try to send a message without blocking. `Err(msg)` if the queue is full.
    pub fn try_send(&self, msg: T) -> Result<(), T> {
        loop {
            let tail = self.tail.load(Ordering::Acquire);
            let head = self.head.load(Ordering::Acquire);
            if tail.wrapping_sub(head) >= N {
                return Err(msg); // full
            }
            // Reserve this position.
            if self
                .tail
                .compare_exchange_weak(tail, tail + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let slot = tail % N;
                unsafe { (*self.slots[slot].get()).as_mut_ptr().write(msg) };
                self.ready[slot].store(true, Ordering::Release);
                return Ok(());
            }
        }
    }

    /// Try to receive a message without blocking. `Err(())` if empty.
    pub fn try_recv(&self) -> Result<T, ()> {
        loop {
            let head = self.head.load(Ordering::Acquire);
            let tail = self.tail.load(Ordering::Acquire);
            if head == tail {
                return Err(()); // empty
            }
            let slot = head % N;
            if !self.ready[slot].load(Ordering::Acquire) {
                // Producer reserved the slot but hasn't published yet.
                return Err(());
            }
            if self
                .head
                .compare_exchange_weak(head, head + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let msg = unsafe { (*self.slots[slot].get()).as_ptr().read() };
                self.ready[slot].store(false, Ordering::Release);
                return Ok(msg);
            }
        }
    }

    /// Send from interrupt context — never blocks.
    pub fn send_isr(&self, msg: T) -> Result<(), T> {
        self.try_send(msg)
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

/// Send with timeout. `timeout_ms == 0` is non-blocking; `u32::MAX` waits
/// forever. Blocks via the kernel waiter list when the queue is full.
pub fn send<T, const N: usize>(
    q: &Queue<T, N>,
    msg: T,
    timeout_ms: u32,
) -> Result<(), SendError<T>> {
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
    loop {
        if !block_send(q.addr(), timeout_ms) {
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
        return Err(RecvError);
    }
    #[cfg(not(test))]
    loop {
        if !block_recv(q.addr(), timeout_ms) {
            return Err(RecvError); // timed out
        }
        if let Ok(m) = q.try_recv() {
            wake_sender(q.addr());
            return Ok(m);
        }
    }
    #[cfg(test)]
    Err(RecvError)
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

/// Error returned by [`recv`] when no message could be received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecvError;

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
        assert_eq!(q.try_recv(), Err(()));
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
        assert_eq!(recv(&q, 0), Err(RecvError));
        assert_eq!(recv(&q, 100), Err(RecvError));
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
    fn recv_error_eq() {
        assert_eq!(RecvError, RecvError);
    }
}
