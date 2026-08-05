// SPDX-License-Identifier: Apache-2.0

//! Bring-up counters, incremented by the demo tasks and reported by the trap
//! handler's heartbeat.
//!
//! These live in the library crate so both the kernel binary (which owns the
//! demo tasks) and `switch.rs` (which prints them) can reach the same statics.

use core::sync::atomic::{AtomicU32, Ordering};

pub static SENSOR: AtomicU32 = AtomicU32::new(0);
pub static CONSUMER: AtomicU32 = AtomicU32::new(0);
pub static HOUSEKEEP: AtomicU32 = AtomicU32::new(0);

pub fn sensor() -> u32 {
    SENSOR.load(Ordering::Relaxed)
}
pub fn consumer() -> u32 {
    CONSUMER.load(Ordering::Relaxed)
}
pub fn housekeep() -> u32 {
    HOUSEKEEP.load(Ordering::Relaxed)
}
