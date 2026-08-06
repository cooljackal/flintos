// SPDX-License-Identifier: Apache-2.0

//! Concurrency tests for `Queue`.
//!
//! Every other test in this crate feeds the queue from one thread, which
//! exercises the state machine but never the thing it was written for. `Queue`
//! declares `unsafe impl Send + Sync` and is reached from an ISR and a task at
//! the same time, so its whole correctness argument is about interleaving —
//! and single-threaded tests cannot disagree with a wrong one.
//!
//! A host has real threads and a real memory model, so these run the actual
//! race. That is not the same concurrency the kernel has (no ISR preempts a
//! task mid-`try_send` here, and x86 reorders far less than Xtensa), so a green
//! run is not proof. What it does catch is the class of bug that survives
//! review: a slot handed to two producers, a message counted twice, a CAS whose
//! failure path drops the payload. Those show up here in seconds and would take
//! a very long time to find on a board.
//!
//! Loops are sized to actually interleave — a run that finishes before the
//! second thread starts proves nothing — while staying quick enough that nobody
//! is tempted to delete them.

// This crate is `no_std`; the threads and collections below exist only on a
// host, and only under `cfg(test)`.
extern crate std;

use std::boxed::Box;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::vec::Vec;

use super::*;

/// Leak a queue so every thread can hold a `&'static` to it.
///
/// The kernel's queues are `static` for real; this matches that rather than
/// inventing a lifetime the type never sees in production.
fn leaked<const N: usize>() -> &'static Queue<u32, N> {
    Box::leak(Box::new(Queue::<u32, N>::new()))
}

#[test]
fn every_message_arrives_exactly_once() {
    const N: usize = 16;
    const PER_PRODUCER: u32 = 2_000;
    const PRODUCERS: u32 = 4;

    let q = leaked::<N>();
    let done = Arc::new(AtomicBool::new(false));

    // Each producer owns a disjoint numeric range, so a duplicate or a
    // fabricated value is identifiable rather than merely suspicious.
    let producers: Vec<_> = (0..PRODUCERS)
        .map(|p| {
            thread::spawn(move || {
                let base = p * PER_PRODUCER;
                for i in 0..PER_PRODUCER {
                    // A full queue is a legitimate outcome, not a failure:
                    // retry rather than drop, or the accounting below is
                    // meaningless.
                    //
                    // Bounded, though. An unbounded retry turns a broken queue
                    // into a hung test, and a CI job that hangs is far worse
                    // than one that fails -- it reports nothing, blocks a
                    // runner, and gets killed by a timeout that names no cause.
                    // Verified: removing the per-slot CAS from `try_send` makes
                    // this loop spin forever, which is exactly what the bound
                    // is here to convert into a legible failure.
                    let mut attempts = 0u32;
                    while q.try_send(base + i).is_err() {
                        attempts += 1;
                        assert!(
                            attempts < 10_000_000,
                            "producer {p} could not place message {i} after \
                             {attempts} attempts -- the queue is losing slots, \
                             not merely full"
                        );
                        thread::yield_now();
                    }
                }
            })
        })
        .collect();

    let done_rx = done.clone();
    let consumer = thread::spawn(move || {
        let mut seen: Vec<u32> = Vec::new();
        loop {
            match q.try_recv() {
                Ok(v) => seen.push(v),
                Err(_) => {
                    if done_rx.load(Ordering::Acquire) {
                        // Drain whatever landed between the last failed recv
                        // and the flag being set.
                        while let Ok(v) = q.try_recv() {
                            seen.push(v);
                        }
                        return seen;
                    }
                    thread::yield_now();
                }
            }
        }
    });

    for p in producers {
        p.join().expect("producer panicked");
    }
    done.store(true, Ordering::Release);
    let seen = consumer.join().expect("consumer panicked");

    let total = (PRODUCERS * PER_PRODUCER) as usize;
    let unique: HashSet<u32> = seen.iter().copied().collect();

    assert_eq!(
        unique.len(),
        seen.len(),
        "a message was delivered more than once: two receivers were handed the \
         same slot"
    );
    assert_eq!(
        seen.len(),
        total,
        "{} of {total} messages went missing -- a producer's payload was \
         dropped on a lost CAS",
        total - seen.len()
    );
    for p in 0..PRODUCERS {
        for i in 0..PER_PRODUCER {
            assert!(
                unique.contains(&(p * PER_PRODUCER + i)),
                "message {} from producer {p} never arrived",
                p * PER_PRODUCER + i
            );
        }
    }
}

#[test]
fn a_full_queue_rejects_rather_than_overwriting() {
    const N: usize = 8;
    let q = leaked::<N>();

    for i in 0..N as u32 {
        assert!(q.try_send(i).is_ok(), "the queue should hold {N}");
    }
    // The rejected value comes back rather than being swallowed, so a caller
    // can retry with it.
    assert_eq!(
        q.try_send(999),
        Err(999),
        "a full queue must hand the message back, not overwrite an unread one"
    );

    for i in 0..N as u32 {
        assert_eq!(q.try_recv(), Ok(i), "FIFO order, nothing clobbered");
    }
}

#[test]
fn concurrent_producers_never_share_a_slot() {
    // A tiny queue maximises contention: with two slots and four producers,
    // every send is a fight for the same memory.
    const N: usize = 2;
    const ROUNDS: u32 = 4_000;

    let q = leaked::<N>();
    let sent = Arc::new(AtomicUsize::new(0));
    let received = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let producers: Vec<_> = (0..4)
        .map(|_| {
            let sent = sent.clone();
            thread::spawn(move || {
                for i in 0..ROUNDS {
                    if q.try_send(i).is_ok() {
                        sent.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();

    let rx_count = received.clone();
    let rx_stop = stop.clone();
    let consumer = thread::spawn(move || {
        while !rx_stop.load(Ordering::Acquire) {
            while q.try_recv().is_ok() {
                rx_count.fetch_add(1, Ordering::Relaxed);
            }
            thread::yield_now();
        }
        while q.try_recv().is_ok() {
            rx_count.fetch_add(1, Ordering::Relaxed);
        }
    });

    for p in producers {
        p.join().expect("producer panicked");
    }
    stop.store(true, Ordering::Release);
    consumer.join().expect("consumer panicked");

    assert_eq!(
        received.load(Ordering::Relaxed),
        sent.load(Ordering::Relaxed),
        "the count that went in and the count that came out disagree: a slot \
         was written twice, or a claimed slot was never delivered"
    );
}

#[test]
fn a_producer_racing_a_consumer_on_one_slot_stays_consistent() {
    // N = 1 is the sharpest case: producer and consumer contend for the same
    // slot on every single operation, so any window between claiming a slot and
    // publishing it is hit immediately rather than occasionally.
    const N: usize = 1;
    const ROUNDS: u32 = 20_000;

    let q = leaked::<N>();
    let received = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let sent = Arc::new(AtomicUsize::new(0));
    let tx_sent = sent.clone();
    let producer = thread::spawn(move || {
        for i in 0..ROUNDS {
            if q.try_send(i).is_ok() {
                tx_sent.fetch_add(1, Ordering::Relaxed);
            }
        }
    });

    let rx_count = received.clone();
    let rx_stop = stop.clone();
    let consumer = thread::spawn(move || {
        let mut last: Option<u32> = None;
        while !rx_stop.load(Ordering::Acquire) {
            if let Ok(v) = q.try_recv() {
                // One producer sends strictly increasing values, so the
                // consumer must observe them in order. Out of order means a
                // slot was published before its payload was written.
                if let Some(prev) = last {
                    assert!(v > prev, "received {v} after {prev}: torn publish");
                }
                last = Some(v);
                rx_count.fetch_add(1, Ordering::Relaxed);
            }
        }
        while let Ok(v) = q.try_recv() {
            if let Some(prev) = last {
                assert!(v > prev, "received {v} after {prev}: torn publish");
            }
            last = Some(v);
            rx_count.fetch_add(1, Ordering::Relaxed);
        }
    });

    producer.join().expect("producer panicked");
    stop.store(true, Ordering::Release);
    consumer.join().expect("consumer panicked");

    assert_eq!(
        received.load(Ordering::Relaxed),
        sent.load(Ordering::Relaxed),
        "every accepted send must be received exactly once"
    );
}

#[test]
fn try_send_never_blocks_even_under_contention() {
    // `send_isr` is called from an interrupt handler, where blocking is not an
    // option: the task it interrupted may be the very consumer that would drain
    // the queue, so waiting for space deadlocks the system. The retry loop
    // inside `try_send` is bounded for this reason, and this checks it stays
    // that way -- an unbounded spin here would hang rather than fail.
    const N: usize = 4;
    let q = leaked::<N>();

    // Fill it, so every send below takes the failure path.
    for i in 0..N as u32 {
        q.try_send(i).unwrap();
    }

    let hammer = thread::spawn(move || {
        for _ in 0..50_000 {
            let _ = q.send_isr(1);
        }
        true
    });

    // A contending consumer keeps the slot states churning under the sender.
    let churn = thread::spawn(move || {
        for _ in 0..50_000 {
            if q.try_recv().is_ok() {
                let _ = q.try_send(0);
            }
        }
    });

    assert!(hammer.join().expect("send_isr hung or panicked"));
    churn.join().expect("consumer panicked");
}
