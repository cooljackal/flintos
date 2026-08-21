// SPDX-License-Identifier: Apache-2.0

#![no_std]

use core::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering};

#[no_mangle]
pub fn u8_compare_exchange(value: &AtomicU8, current: u8, new: u8) -> bool {
    value
        .compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

#[no_mangle]
pub fn u32_compare_exchange(value: &AtomicU32, current: u32, new: u32) -> bool {
    value
        .compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

#[no_mangle]
pub fn usize_fetch_add(value: &AtomicUsize, increment: usize) -> usize {
    value.fetch_add(increment, Ordering::SeqCst)
}

