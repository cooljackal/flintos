// SPDX-License-Identifier: Apache-2.0

//! Priority-inheritance mutex.
//!
//! A mutex whose owner blocks until the lock is acquired, and whose
//! priority is temporarily elevated if a higher-priority task is
//! waiting (priority inheritance — prevents inversion).
//!
//! # Example
//!
//! ```ignore
//! use api::mutex::{Mutex, lock};
//!
//! static DATA: Mutex<u32> = Mutex::new(0);
//!
//! let mut guard = lock(&DATA);
//! *guard = 42;
//! // guard is dropped here, releasing the lock
//! ```

use core::cell::UnsafeCell;

/// A mutex with priority inheritance.
pub struct Mutex<T> {
    data: UnsafeCell<T>,
    #[allow(dead_code)]
    owner: core::sync::atomic::AtomicU32,
    #[allow(dead_code)]
    original_prio: core::sync::atomic::AtomicU8,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    /// Create a new mutex wrapping `value`.
    pub const fn new(value: T) -> Self {
        Self {
            data: UnsafeCell::new(value),
            owner: core::sync::atomic::AtomicU32::new(u32::MAX),
            original_prio: core::sync::atomic::AtomicU8::new(0),
        }
    }
}

/// A guard that releases the mutex when dropped.
///
/// Deliberately **not** `Send` (item 7): the kernel's `unlock` transfers
/// ownership by looking up the *current* task, so the unlock must happen on
/// whichever task actually performed the `lock`. Moving a guard to another
/// task and dropping it there would release the mutex on behalf of a task
/// that never held it — std's `MutexGuard` opts out of `Send` for the same
/// reason. `PhantomData<*const ()>` is a standard zero-cost way to make a
/// type `!Send` (raw pointers are not `Send`) without otherwise affecting
/// layout or `Deref`/`Drop` behavior.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
    _not_send: core::marker::PhantomData<*const ()>,
}

// Compile-time regression guard for the above: fails to compile if
// `MutexGuard` is ever `Send` again. This is the same "ambiguous blanket
// impl" trick `static_assertions::assert_not_impl_any!` uses, reimplemented
// locally so this `no_std`, dependency-light crate doesn't need to pull in
// that crate just for one assertion. If `MutexGuard<'static, u32>` were
// `Send`, both impls of `AmbiguousIfSend` below would apply to it and the
// call at the bottom would fail with an ambiguous-associated-item error.
#[allow(dead_code)]
const _MUTEX_GUARD_NOT_SEND: fn() = || {
    trait AmbiguousIfSend<A> {
        fn some_item() {}
    }
    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    struct Invalid;
    impl<T: ?Sized + Send> AmbiguousIfSend<Invalid> for T {}
    let _ = <MutexGuard<'static, u32> as AmbiguousIfSend<_>>::some_item;
};

impl<T> core::ops::Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> core::ops::DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        #[cfg(not(test))]
        unsafe {
            extern "Rust" {
                fn _flint_sys_mutex_unlock(mutex: *const core::ffi::c_void);
            }
            _flint_sys_mutex_unlock(self.mutex as *const _ as *const core::ffi::c_void);
        }
    }
}

/// Retry bound for [`lock`]'s yield loop (item 12). The kernel's mutex table
/// only frees a slot when `unlock` finds no waiters; an abandoned mutex (or a
/// misuse the kernel refused and logged, e.g. the re-entrant-lock and
/// not-the-owner cases above) can leak a slot permanently. Once every slot is
/// stuck, every `lock` call would otherwise spin here forever with no
/// diagnostic — exactly the silent-hang the project's philosophy forbids.
/// Bounding it turns that into a loud, debuggable failure instead.
#[cfg_attr(test, allow(dead_code))] // only read from the #[cfg(not(test))] retry loop below
const MAX_LOCK_RETRIES: u32 = 100_000;

/// Lock `mutex`, blocking until acquired.
///
/// The kernel returns `false` only if the lock could not even be queued
/// (mutex/waiter table full, interrupt-context misuse, or re-entrant-lock
/// misuse — all logged on the kernel side). In that case we yield and retry
/// rather than silently handing out a guard we don't own (plan W3.5), up to
/// [`MAX_LOCK_RETRIES`]; exceeding that means the mutex/waiter tables are
/// permanently exhausted (item 12), and we panic loudly rather than hang the
/// task forever with no diagnostic.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    #[cfg(not(test))]
    unsafe {
        extern "Rust" {
            fn _flint_sys_mutex_lock(mutex: *const core::ffi::c_void) -> bool;
            fn _flint_sys_yield();
        }
        let addr = mutex as *const _ as *const core::ffi::c_void;
        let mut attempts: u32 = 0;
        while !_flint_sys_mutex_lock(addr) {
            attempts += 1;
            if attempts >= MAX_LOCK_RETRIES {
                crate::flint_panic!(
                    "mutex::lock: gave up after {} retries (mutex/waiter table exhausted?)",
                    MAX_LOCK_RETRIES
                );
            }
            _flint_sys_yield();
        }
    }
    MutexGuard { mutex, _not_send: core::marker::PhantomData }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;

    use core::mem::ManuallyDrop;
    use super::*;

    #[test]
    fn mutex_guard_deref() {
        static M: Mutex<u32> = Mutex::new(123);
        let guard = ManuallyDrop::new(MutexGuard { mutex: &M, _not_send: core::marker::PhantomData });
        assert_eq!(**guard, 123);
    }

    #[test]
    fn mutex_guard_deref_mut() {
        static M: Mutex<u32> = Mutex::new(0);
        let mut guard = ManuallyDrop::new(MutexGuard { mutex: &M, _not_send: core::marker::PhantomData });
        **guard = 42;
        assert_eq!(**guard, 42);
    }

    #[test]
    fn mutex_new_initialises() {
        let m = Mutex::new(7u32);
        let val = unsafe { *m.data.get() };
        assert_eq!(val, 7);
    }

}
