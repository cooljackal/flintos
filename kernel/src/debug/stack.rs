// SPDX-License-Identifier: Apache-2.0

//! Stack high-water marking and overflow detection.
//!
//! Every task stack is painted with `STACK_PAINT` at spawn, except its lowest
//! word which holds `STACK_GUARD`. Stacks grow downward, so untouched memory
//! accumulates at the *low* end and the guard is the first thing an overflow
//! destroys.

use crate::scheduler;
use crate::spawn::stack_guard_intact;

const STACK_PAINT: u32 = 0xDEADBEEF;

/// Fraction of the stack above which a warning is emitted, in percent.
const WARN_PCT: u32 = 80;

/// Update the high-water mark for a task and check its guard word.
///
/// Takes the scheduler rather than reaching for it, because the caller already
/// holds the lock. Reaching for it here would mint a second `&mut` to data the
/// caller is already holding one to — sound on one core only by accident, and
/// aliasing UB in the language regardless.
pub fn update_hwm(sched: &mut scheduler::Scheduler, task_id: u32) {
    let Some(tcb) = &mut sched.tasks[task_id as usize] else {
        return;
    };
    if tcb.stack_size == 0 {
        return; // idle runs on the boot stack; nothing painted to scan
    }

    let base = tcb.stack_base;
    let size = tcb.stack_size;
    let name = tcb.name;

    let used = used_bytes(base, size);
    if used > tcb.stack_hwm {
        tcb.stack_hwm = used;

        // Only report on a new maximum, so a task sitting near the limit does
        // not flood the console every tick.
        if (used * 100) / size >= WARN_PCT {
            crate::debug::fault::raw_print("[FLINT] WARNING: stack high-water >");
            crate::debug::fault::raw_print(" 80% for task ");
            crate::debug::fault::raw_print(name);
            crate::debug::fault::raw_print("\r\n");
        }
    }

    if !stack_guard_intact(base, size) {
        // Past this point the task has already written below its own stack and
        // corrupted whatever lies beneath. Nothing here can repair that; the
        // value is in saying so, because the symptom otherwise appears as
        // unrelated corruption somewhere else entirely.
        crate::debug::fault::raw_print("[FLINT] FATAL: stack overflow in task ");
        crate::debug::fault::raw_print(name);
        crate::debug::fault::raw_print("\r\n");
        panic!("task stack overflow");
    }
}

/// Bytes of `base..base+size` that have been written since the stack was
/// painted.
///
/// Counts the run of untouched paint words upward from the base; everything
/// above the run has been used. The previous implementation returned the index
/// of the *first* painted word, which is zero for any freshly painted stack, so
/// the high-water mark never moved off zero.
fn used_bytes(base: u32, size: u32) -> u32 {
    // The `u32` address is a target fact — stacks live in the low 32 bits of
    // the ESP32 map, and the TCB stores them as `u32`. The scan itself is just
    // pointer arithmetic, so it is split out: a host test can hand it the
    // address of a real array, which it cannot do through a `u32`. On a 64-bit
    // host `array.as_ptr() as u32` truncates, and reconstructing a pointer from
    // the truncated value dereferences somewhere unrelated.
    used_bytes_at(base as *const u32, size)
}

/// # Safety
/// `ptr` must be valid for reads of `size` bytes.
fn used_bytes_at(ptr: *const u32, size: u32) -> u32 {
    let words = (size / 4) as usize;
    let mut untouched = 0usize;
    // Word 0 is the guard, not paint, so start the scan above it.
    for i in 1..words {
        let w = unsafe { ptr.add(i).read_volatile() };
        if w != STACK_PAINT {
            break;
        }
        untouched += 1;
    }
    // Measure against the *usable* stack, which excludes the guard word.
    //
    // Subtracting from `size` counted the guard as used, so a task that had
    // never run reported 4 bytes of usage and every high-water mark was one
    // word high. Harmless at 4 KiB, but `update_hwm` divides by `size` to
    // decide when to warn at 80%, and the smaller the stack the more the
    // constant offset skews that. The unit test above asserted the correct
    // values all along; it had simply never been compiled, because this crate
    // could not be built for a host until `arch.rs` existed.
    let usable = size.saturating_sub(4);
    usable.saturating_sub((untouched * 4) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_is_distinct_from_paint() {
        // If these ever collided, an overflow that happened to write the paint
        // value would look like an intact guard.
        assert_ne!(crate::spawn::STACK_GUARD, STACK_PAINT);
    }

    #[test]
    fn used_bytes_counts_from_the_top_down() {
        // Through `used_bytes_at`, not `used_bytes`: the latter takes a `u32`
        // address, and on a 64-bit host `stack.as_ptr() as u32` truncates to an
        // address that is not this array. This test never ran until the kernel
        // became host-testable, and when it first did it dereferenced that
        // truncated pointer and took the whole suite down with an access
        // violation.
        let mut stack = [STACK_PAINT; 16];
        stack[0] = crate::spawn::STACK_GUARD;
        assert_eq!(used_bytes_at(stack.as_ptr(), 64), 0);

        // Writing the top two words marks 8 bytes used.
        stack[15] = 0x1234;
        stack[14] = 0x5678;
        assert_eq!(used_bytes_at(stack.as_ptr(), 64), 8);
    }
}
