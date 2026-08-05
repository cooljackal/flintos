//! Priority-inheritance mutex.
//!
//! A mutex whose owner blocks until the lock is acquired, and whose
//! priority is temporarily elevated if a higher-priority task is
//! waiting (priority inheritance — prevents inversion).
//!
//! # Example
//!
//! ```ignore
//! use flint_api::mutex::{Mutex, lock};
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
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

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

/// Lock `mutex`, blocking until acquired.
///
/// The kernel returns `false` only if the lock could not even be queued (mutex
/// or waiter table full). In that case we yield and retry rather than silently
/// handing out a guard we don't own (plan W3.5).
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    #[cfg(not(test))]
    unsafe {
        extern "Rust" {
            fn _flint_sys_mutex_lock(mutex: *const core::ffi::c_void) -> bool;
            fn _flint_sys_yield();
        }
        let addr = mutex as *const _ as *const core::ffi::c_void;
        while !_flint_sys_mutex_lock(addr) {
            _flint_sys_yield();
        }
    }
    MutexGuard { mutex }
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
        let guard = ManuallyDrop::new(MutexGuard { mutex: &M });
        assert_eq!(**guard, 123);
    }

    #[test]
    fn mutex_guard_deref_mut() {
        static M: Mutex<u32> = Mutex::new(0);
        let mut guard = ManuallyDrop::new(MutexGuard { mutex: &M });
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
