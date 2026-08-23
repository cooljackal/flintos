// SPDX-License-Identifier: Apache-2.0

//! Static storage that is safe to share: [`Once`] for a value written once and
//! read forever, [`CsCell`] for a value a task and an interrupt handler both
//! touch.
//!
//! Both exist to retire the `static mut X: Option<T>` + `addr_of!` pattern.
//! That pattern compiles, but nothing checks that the write happened before
//! the read, that it happened only once, or that an interrupt did not land in
//! the middle of it.
//!
//! # Example
//!
//! ```ignore
//! use api::sync::{CsCell, Once};
//!
//! static BUS: Once<SpiBus> = Once::new();
//! static PENDING: CsCell<u32> = CsCell::new(0);
//!
//! fn main() {
//!     let bus = BUS.init(SpiBus::new(..));   // &'static SpiBus
//!     PENDING.with(|n| *n += 1);             // interrupts masked inside
//! }
//! ```

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use portable_atomic::{AtomicU8, Ordering};

// ── Once ────────────────────────────────────────────────────────────────────

/// Empty: no writer has claimed the cell.
const EMPTY: u8 = 0;
/// Claimed: one writer is moving the value in; readers must wait or bail.
const INITIALIZING: u8 = 1;
/// Full: the value is in place and will never change again.
const READY: u8 = 2;

/// A value that is written once and then readable by everyone for the rest
/// of the program.
///
/// Meant for a `static`: every method that hands out a reference takes
/// `&'static self`, so the reference it returns can also be `'static` — a
/// driver instance built in `main` and then used from a task, say.
///
/// The state word is a `portable_atomic::AtomicU8` rather than
/// `core::sync::atomic`: the RP2040's Cortex-M0+ has no compare-and-swap
/// instruction, and `portable-atomic` falls back to a critical section there.
///
/// # Safety argument
///
/// The `UnsafeCell<MaybeUninit<T>>` is only ever:
///
/// - **written** by the one caller that won the `EMPTY → INITIALIZING`
///   compare-exchange. Nobody else can win it again, so there is exactly one
///   writer for the life of the program, and it holds the only `&mut` while
///   the state is `INITIALIZING`.
/// - **read** after observing `READY` with `Acquire` ordering, which pairs
///   with the writer's `Release` store and makes the written value visible.
///   Once `READY`, the value is never written again, so every `&T` handed out
///   is a shared borrow of immutable memory.
///
/// A reader that sees `INITIALIZING` gets `None` rather than a reference —
/// it never touches the cell. That is also why `Sync` is sound for
/// `T: Sync`: the only thing two threads can do concurrently through this type
/// is read a `T` that is immutable from then on.
///
/// `T: Send` is not required: the value is constructed in place by the
/// writer and never moved out, so no ownership crosses threads.
pub struct Once<T> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<T>>,
}

// See the safety argument on the type.
unsafe impl<T: Sync> Sync for Once<T> {}

impl<T> Once<T> {
    /// An empty cell.
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(EMPTY),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Store `value` and return a reference to it.
    ///
    /// # Panics
    ///
    /// If the cell was already initialised, or another caller is initialising
    /// it right now. A second initialisation is a program bug — two pieces of
    /// code both think they own the resource — and silently keeping either
    /// value would hide it.
    pub fn init(&'static self, value: T) -> &'static T {
        match self.get_or_try_init(|| Ok::<T, core::convert::Infallible>(value)) {
            Ok(v) => v,
            Err(never) => match never {},
        }
    }

    /// The stored value, or `None` if nothing has been stored yet.
    pub fn get(&'static self) -> Option<&'static T> {
        if self.state.load(Ordering::Acquire) == READY {
            // SAFETY: READY was observed with Acquire, pairing with the
            // writer's Release store; the value is initialised and will never
            // be written again.
            Some(unsafe { (*self.value.get()).assume_init_ref() })
        } else {
            None
        }
    }

    /// Store the value `f` produces, unless one is already stored.
    ///
    /// `f` runs only when the cell is empty. If it fails, the cell stays
    /// empty and its error is returned, so a later call can try again.
    ///
    /// # Panics
    ///
    /// If the cell is already initialised, or is being initialised by
    /// another caller — see [`init`](Self::init). `get_or_try_init` is for
    /// "initialise unless I already did"; reaching it from two places at once
    /// is the bug it exists to surface.
    pub fn get_or_try_init<E>(
        &'static self,
        f: impl FnOnce() -> Result<T, E>,
    ) -> Result<&'static T, E> {
        match self
            .state
            .compare_exchange(EMPTY, INITIALIZING, Ordering::Acquire, Ordering::Acquire)
        {
            Ok(_) => {}
            Err(READY) => panic!("api::sync::Once: initialised twice"),
            Err(_) => panic!("api::sync::Once: initialised from two places at once"),
        }
        match f() {
            Ok(v) => {
                // SAFETY: we won the compare-exchange, so we are the only
                // writer and no reader touches the cell while the state is
                // INITIALIZING.
                unsafe { (*self.value.get()).write(v) };
                self.state.store(READY, Ordering::Release);
                // SAFETY: just written, and READY is now published.
                Ok(unsafe { (*self.value.get()).assume_init_ref() })
            }
            Err(e) => {
                // Nothing was written; give the cell back.
                self.state.store(EMPTY, Ordering::Release);
                Err(e)
            }
        }
    }
}

impl<T> Default for Once<T> {
    fn default() -> Self {
        Self::new()
    }
}

// ── CsCell ──────────────────────────────────────────────────────────────────

/// A value shared between a task and an interrupt handler.
///
/// Every access goes through [`with`](Self::with), which masks interrupts
/// for the duration of the closure using the kernel's critical section (the
/// same one the scheduler uses). That is what makes it safe for both sides:
/// the ISR cannot land while the task is half-way through an update, and
/// the task cannot be reading while the ISR writes.
///
/// Keep the closure short — nothing else runs while it does. Do not call
/// anything that blocks (`sleep_ms`, a queue receive, a mutex lock) inside
/// it.
///
/// For a value written once and then only read, use [`Once`] instead; it
/// costs nothing on the read path.
///
/// # Safety argument
///
/// The `&mut T` handed to the closure is the only reference to the value
/// that exists while interrupts are masked. A critical section is exclusive
/// on a single core: no other task runs (the scheduler cannot preempt) and no
/// interrupt handler runs. Nesting a `with` inside another `with` on the same
/// cell is prevented by the borrow checker for the common case (the outer
/// closure's `&mut T` is live) and, where it is not, is the caller's bug —
/// the same rule `RefCell` enforces at runtime, and the critical section
/// would still serialise the two.
///
/// `Sync` for `T: Send`: the value crosses between task and ISR contexts, so
/// it must be `Send`; it never needs to be `Sync` because no two contexts
/// ever hold a reference at the same time.
pub struct CsCell<T> {
    value: UnsafeCell<T>,
}

// See the safety argument on the type.
unsafe impl<T: Send> Sync for CsCell<T> {}

impl<T> CsCell<T> {
    /// Wrap `value`.
    pub const fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(value),
        }
    }

    /// Run `f` on the value with interrupts masked.
    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let _cs = CriticalSection::enter();
        // SAFETY: interrupts are masked for the life of `_cs`, so neither an
        // ISR nor another task can be inside another `with` on this cell.
        f(unsafe { &mut *self.value.get() })
    }
}

impl<T: Default> Default for CsCell<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// Token for the kernel's critical section; leaving it restores the previous
/// interrupt state.
///
/// Entered through the `_flint_sys_cs_enter` / `_flint_sys_cs_exit` pair, so
/// this crate stays architecture-agnostic: the kernel picks the
/// implementation in `kernel::arch`. On the host there is nothing to mask, so
/// the test build is a pass-through, as `kernel::arch::host::cs_with` is.
struct CriticalSection {
    #[cfg(not(test))]
    saved: u32,
}

impl CriticalSection {
    fn enter() -> Self {
        #[cfg(not(test))]
        {
            extern "Rust" {
                fn _flint_sys_cs_enter() -> u32;
            }
            Self {
                saved: unsafe { _flint_sys_cs_enter() },
            }
        }
        #[cfg(test)]
        {
            Self {}
        }
    }
}

impl Drop for CriticalSection {
    fn drop(&mut self) {
        #[cfg(not(test))]
        {
            extern "Rust" {
                fn _flint_sys_cs_exit(saved: u32);
            }
            unsafe { _flint_sys_cs_exit(self.saved) }
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn once_starts_empty() {
        static C: Once<u32> = Once::new();
        assert!(C.get().is_none());
    }

    #[test]
    fn once_init_then_get() {
        static C: Once<u32> = Once::new();
        let r = C.init(7);
        assert_eq!(*r, 7);
        assert_eq!(C.get().copied(), Some(7));
        // Same storage each time.
        assert!(core::ptr::eq(r, C.get().unwrap()));
    }

    #[test]
    #[should_panic(expected = "initialised twice")]
    fn once_second_init_panics() {
        static C: Once<u32> = Once::new();
        C.init(1);
        C.init(2);
    }

    #[test]
    fn once_try_init_error_leaves_cell_empty() {
        static C: Once<u32> = Once::new();
        let r: Result<&u32, &str> = C.get_or_try_init(|| Err("not yet"));
        assert_eq!(r, Err("not yet"));
        assert!(C.get().is_none());
        // And the cell is still usable afterwards.
        let r: Result<&u32, &str> = C.get_or_try_init(|| Ok(3));
        assert_eq!(r.copied(), Ok(3));
    }

    #[test]
    #[should_panic(expected = "initialised twice")]
    fn once_try_init_after_init_panics() {
        static C: Once<u32> = Once::new();
        C.init(1);
        let _ = C.get_or_try_init(|| Ok::<u32, ()>(2));
    }

    #[test]
    fn once_holds_non_copy_value() {
        struct Driver {
            regs: std::vec::Vec<u8>,
        }
        static C: Once<Driver> = Once::new();
        let d = C.init(Driver { regs: std::vec![1, 2, 3] });
        assert_eq!(d.regs.len(), 3);
        assert_eq!(C.get().unwrap().regs[2], 3);
    }

    #[test]
    fn once_is_sync_for_sync_t() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Once<u32>>();
        assert_sync::<CsCell<u32>>();
    }

    #[test]
    fn cs_cell_with_mutates_and_returns() {
        static C: CsCell<u32> = CsCell::new(0);
        let before = C.with(|v| {
            let b = *v;
            *v += 5;
            b
        });
        assert_eq!(before, 0);
        assert_eq!(C.with(|v| *v), 5);
    }

    #[test]
    fn cs_cell_holds_option_for_late_init() {
        static S: CsCell<Option<std::string::String>> = CsCell::new(None);
        S.with(|s| *s = Some(std::string::String::from("isr")));
        assert_eq!(S.with(|s| s.as_deref().map(|x| x.len())), Some(3));
    }
}
