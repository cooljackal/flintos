// SPDX-License-Identifier: Apache-2.0

//! Wall-clock time at microsecond resolution.
//!
//! [`now_us`] sits beside [`timer::now_ms`](crate::timer::now_ms): the
//! millisecond form is the scheduler tick and is all a task usually needs,
//! while this one reads the free-running hardware counter for code that has to
//! measure something shorter than a tick — a bus turnaround, an interrupt
//! latency — or timestamp from trap context, where the tick lock is not safe
//! to take.

/// Microseconds since boot, monotonic.
///
/// One hardware counter, so it counts up and never goes back, including across
/// a task switch on either core. Where the SoC has no such counter it falls
/// back to the millisecond tick scaled up — still monotonic and correctly
/// ordered, a thousand times coarser — so compare readings for *ordering*, not
/// for sub-millisecond *resolution* you may not have.
pub fn now_us() -> u64 {
    extern "Rust" {
        fn _flint_sys_now_us() -> u64;
    }
    unsafe { _flint_sys_now_us() }
}
