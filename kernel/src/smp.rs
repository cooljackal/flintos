// SPDX-License-Identifier: Apache-2.0

//! Mutual exclusion between cores.
//!
//! A [`Spinlock`] guards data shared by more than one core. It is not the
//! kernel's task-level [`crate::mutex`]: that blocks the caller and involves
//! the scheduler, which is precisely what cannot be relied on here — the
//! scheduler is one of the things this protects.
//!
//! # Masking comes first, always
//!
//! [`Spinlock::with`] masks interrupts on the calling core **before** it tries
//! to acquire, and unmasks after releasing. That order is the whole
//! correctness argument, and reversing it deadlocks a single core against
//! itself:
//!
//! 1. a task acquires the lock,
//! 2. an interrupt arrives on that same core,
//! 3. the handler tries to acquire the same lock,
//! 4. it spins, waiting for a task that cannot run until the handler returns.
//!
//! Masking first makes step 2 impossible. It also means a lock is never held
//! across a context switch on the holding core, which is why holding one must
//! stay short.
//!
//! The other core spinning is fine and expected — it is a real wait for a real
//! holder, and it ends.
//!
//! # What this is not
//!
//! Not fair and not reentrant. Reentrancy is a bug rather than a feature, and
//! [`Spinlock::with`] detects it rather than deadlocking silently.
//!
//! It *is* released if the closure unwinds. On the target that hardly matters —
//! a kernel that panics halts — but on a host the tests panic routinely, and a
//! lock leaked by one failing assertion turns every later test into a hang.
//! Which is exactly what happened before the release moved into a `Drop`
//! guard: one broken assert took the whole suite down with it, and a hang says
//! far less than a failure does.

use core::cell::UnsafeCell;
use portable_atomic::{AtomicU8, Ordering};

use hal::smp::{CoreId, MultiCore};

use crate::arch::Smp;

/// No core holds the lock.
///
/// A sentinel rather than a separate `AtomicBool`, so acquiring is one atomic
/// operation and the holder's identity comes along with it — which is what
/// makes reentrancy detectable rather than a hang.
const UNLOCKED: u8 = u8::MAX;

/// Data shared between cores.
pub struct Spinlock<T> {
    holder: AtomicU8,
    data: UnsafeCell<T>,
}

// Safe because every access goes through `with`, which holds the lock for the
// duration. `T: Send` is required because the data really does move between
// cores; `Sync` is not, because only one core ever sees it at a time.
unsafe impl<T: Send> Sync for Spinlock<T> {}
unsafe impl<T: Send> Send for Spinlock<T> {}

impl<T> Spinlock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            holder: AtomicU8::new(UNLOCKED),
            data: UnsafeCell::new(data),
        }
    }

    /// Run `f` with exclusive access, on this core and every other.
    ///
    /// Interrupts are masked on the calling core for the whole of `f`. Keep it
    /// short: the tick cannot be serviced here, and on the other core anyone
    /// waiting is spinning.
    ///
    /// # Panics
    /// If the calling core already holds this lock. That would otherwise spin
    /// forever against itself, and a hang reports nothing — a panic at least
    /// names the file and line that did it.
    /// `inline(always)` so this lands wherever the caller does.
    ///
    /// The second core has no instruction cache and cannot fetch from flash.
    /// A caller in `.iram1` that made a real call here would jump into flash
    /// and hang -- which it did, on the first attempt: the APP CPU incremented
    /// its counter once and stopped dead at the lock. Inlining keeps the whole
    /// acquire/release in the caller's section.
    #[inline(always)]
    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        crate::arch::cs_with(|| {
            let me = Smp::context_id();

            // Reentrancy is a deadlock, not a wait. Check before spinning, so
            // the report happens instead of the hang.
            if self.holder.load(Ordering::Relaxed) == me {
                panic!("spinlock: core {me} already holds this lock");
            }

            // On RP2040 `cs_with` already holds the kernel hardware spinlock.
            // Waiting on this software owner while holding that hardware lock
            // deadlocks: the owner needs the same hardware lock to execute its
            // portable-atomic release. Once inside `cs_with`, an owner from
            // another core has necessarily finished and released this byte.
            #[cfg(target_arch = "arm")]
            self.holder.store(me, Ordering::Relaxed);
            #[cfg(not(target_arch = "arm"))]
            while self
                .holder
                .compare_exchange_weak(UNLOCKED, me, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                core::hint::spin_loop();
            }

            // Released by the guard's `Drop`, not by a statement after the
            // call, so an unwinding closure still frees it.
            let _release = Release { lock: self };

            // Exclusive: this core holds the lock and interrupts are masked on
            // it, so nothing else can reach the data.
            f(unsafe { &mut *self.data.get() })
        })
    }

    /// Acquire only if the lock is free, and say so if it is not.
    ///
    /// For paths that must not block — a fault handler that wants to report
    /// scheduler state without hanging if the scheduler is mid-update.
    #[inline(always)]
    pub fn try_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        crate::arch::cs_try_with(|| {
            let me = Smp::context_id();
            if self.holder.load(Ordering::Relaxed) == me {
                return None;
            }
            #[cfg(target_arch = "arm")]
            self.holder.store(me, Ordering::Relaxed);
            #[cfg(not(target_arch = "arm"))]
            if self
                .holder
                .compare_exchange(UNLOCKED, me, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                return None;
            }
            let _release = Release { lock: self };
            Some(f(unsafe { &mut *self.data.get() }))
        })
        .flatten()
    }

    /// Which execution context holds this, if any. Diagnostics only — the
    /// answer can be stale before it is read.
    ///
    /// A context id, not a [`CoreId`]: on hardware they are the same number,
    /// and on a host they are not.
    pub fn holder(&self) -> Option<u8> {
        match self.holder.load(Ordering::Relaxed) {
            UNLOCKED => None,
            c => Some(c),
        }
    }
}

/// Releases a [`Spinlock`] however the closure leaves — returning, or
/// unwinding out of a failed assertion.
struct Release<'a, T> {
    lock: &'a Spinlock<T>,
}

impl<T> Drop for Release<'_, T> {
    #[inline(always)]
    fn drop(&mut self) {
        self.lock.holder.store(UNLOCKED, Ordering::Release);
    }
}

/// The calling core.
pub fn current_core() -> CoreId {
    Smp::current_core()
}

/// How many cores this build expects to run the kernel.
pub fn cores() -> u8 {
    Smp::cores()
}

/// One bit for every core that has completed its scheduler join.
///
/// Core 0 owns kernel bring-up and is joined from reset. A secondary core sets
/// its bit only after its pinned idle TCB and per-core `current` entry exist;
/// accepting affinity before that point strands tasks indefinitely.
static JOINED_CORES: AtomicU8 = AtomicU8::new(1 << CoreId::BOOT.0);

/// Record that `core` has a valid scheduler context and can accept work.
///
/// Call this after installing that core's idle/current state and before
/// unmasking its scheduling interrupts.
pub fn mark_joined(core: CoreId) {
    assert!(
        core.index() < hal::smp::MAX_CORES && core.0 < cores(),
        "joined core is out of range"
    );
    JOINED_CORES.fetch_or(1u8 << core.0, Ordering::Release);
}

/// The current scheduler membership bitmap.
pub fn joined_mask() -> u8 {
    JOINED_CORES.load(Ordering::Acquire)
}

/// Whether this core is the sole writer of shared kernel time.
pub fn is_timekeeper() -> bool {
    current_core().is_boot()
}

/// Whether a task may be pinned to `core`.
///
/// This exists so `spawn_on` can *refuse* a core that would never run the
/// task. A pinned task that is silently never scheduled is the worst outcome
/// available: it looks like a spawn that worked.
///
/// Three ways to say no, and they are different failures worth separating from
/// the allocation failures that follow: the core is beyond `MAX_CORES`, the
/// part does not have it, or it has it but nothing there runs the scheduler.
pub fn is_pinnable(core: u8) -> bool {
    (core as usize) < hal::smp::MAX_CORES && core < cores() && joined_mask() & (1u8 << core) != 0
}

#[cfg(test)]
pub(crate) fn reset_joined_cores() {
    JOINED_CORES.store(1 << CoreId::BOOT.0, Ordering::Release);
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;
    use std::vec::Vec;

    #[test]
    fn only_a_core_that_schedules_can_be_pinned_to() {
        let _k = crate::testsupport::lock();
        // A core is pinnable once it schedules. Core 0 always does; core 1
        // does after platform bring-up installs its vector table, idle task,
        // and tick. Pinning to a core that never schedules would produce a
        // task that looks spawned and never runs, so the joined mask refuses
        // it until initialization is complete.
        reset_joined_cores();
        assert!(is_pinnable(0));
        assert!(!is_pinnable(hal::smp::MAX_CORES as u8), "past the end");
        assert!(!is_pinnable(1), "core 1 has not joined");
        mark_joined(CoreId(1));
        assert!(is_pinnable(1), "joined core 1 refused affinity");
        assert_eq!(joined_mask().count_ones(), 2);
    }

    #[test]
    fn a_lock_is_free_until_taken() {
        let l = Spinlock::new(0u32);
        assert_eq!(l.holder(), None);
        l.with(|v| *v = 5);
        assert_eq!(l.holder(), None, "released on the way out");
        l.with(|v| assert_eq!(*v, 5));
    }

    #[test]
    fn the_holder_is_recorded_while_held() {
        let l = Spinlock::new(());
        l.with(|_| {
            assert_eq!(l.holder(), Some(crate::arch::Smp::context_id()));
        });
    }

    #[test]
    #[should_panic(expected = "already holds this lock")]
    fn taking_a_lock_twice_on_one_core_panics_rather_than_hanging() {
        // The failure this replaces is a hang, which reports nothing at all.
        let l = Spinlock::new(0u32);
        l.with(|_| {
            l.with(|_| unreachable!("the inner acquire must not succeed"));
        });
    }

    #[test]
    fn try_with_declines_rather_than_hanging_on_reentry() {
        let l = Spinlock::new(7u32);
        l.with(|_| {
            assert_eq!(l.try_with(|_| ()), None);
        });
        assert_eq!(l.try_with(|v| *v), Some(7));
    }

    #[test]
    fn concurrent_increments_are_not_lost() {
        // The property the whole thing exists for. Without the lock this
        // races and the total comes out short; the failure is silent and
        // load-dependent, which is why it is worth a real-threads test rather
        // than a reasoned argument.
        // Exactly `MAX_CORES`, because a host thread models a core and ids
        // wrap. More threads would share an id and trip the reentrancy check
        // on honest contention. Iterations raised to keep the stress.
        // Context ids are unique per thread, so this is free to use more
        // threads than the part has cores.
        const THREADS: usize = 8;
        const EACH: u32 = 20_000;

        let lock = Arc::new(Spinlock::new(0u32));
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let lock = Arc::clone(&lock);
                std::thread::spawn(move || {
                    for _ in 0..EACH {
                        lock.with(|v| *v += 1);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        lock.with(|v| assert_eq!(*v, THREADS as u32 * EACH));
    }

    #[test]
    fn only_one_thread_is_inside_at_a_time() {
        // Counting the total is not enough: a lock that serialised nothing
        // could still add up if the increments happened to interleave safely.
        // This catches an overlap directly.
        const THREADS: usize = 8;
        const EACH: u32 = 5_000;

        struct Shared {
            inside: AtomicU32,
            overlaps: AtomicU32,
        }
        let s = Arc::new(Spinlock::new(()));
        let shared = Arc::new(Shared {
            inside: AtomicU32::new(0),
            overlaps: AtomicU32::new(0),
        });

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let (s, shared) = (Arc::clone(&s), Arc::clone(&shared));
                std::thread::spawn(move || {
                    for _ in 0..EACH {
                        s.with(|_| {
                            if shared.inside.fetch_add(1, Ordering::SeqCst) != 0 {
                                shared.overlaps.fetch_add(1, Ordering::SeqCst);
                            }
                            shared.inside.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            shared.overlaps.load(Ordering::SeqCst),
            0,
            "two threads were inside"
        );
    }

    #[test]
    fn a_lock_survives_being_hammered_by_try_and_blocking_callers_together() {
        // try_with must not corrupt the holder field when it loses the race.
        const EACH: u32 = 20_000;
        let lock = Arc::new(Spinlock::new(0u32));
        let taken = Arc::new(AtomicU32::new(0));

        let blocking = {
            let lock = Arc::clone(&lock);
            std::thread::spawn(move || {
                for _ in 0..EACH {
                    lock.with(|v| *v += 1);
                }
            })
        };
        let trying = {
            let (lock, taken) = (Arc::clone(&lock), Arc::clone(&taken));
            std::thread::spawn(move || {
                for _ in 0..EACH {
                    if lock.try_with(|v| *v += 1).is_some() {
                        taken.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        };
        blocking.join().unwrap();
        trying.join().unwrap();

        let got = lock.with(|v| *v);
        assert_eq!(
            got,
            EACH + taken.load(Ordering::Relaxed),
            "an increment was lost"
        );
        assert_eq!(lock.holder(), None, "left locked");
    }
}
