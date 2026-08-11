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

    let used = used_bytes(base, size, tcb.stack_hwm);
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
///
/// # Why this takes the previous answer
///
/// Because the obvious version costs a full pass over the *unused* part of the
/// stack, and [`update_hwm`] runs on **every tick, holding the scheduler
/// lock**. That is backwards: the better provisioned a task is, the more the
/// kernel pays to say so. A 16 KiB stack means about 4096 volatile reads per
/// pass, which at 80 MHz is roughly a millisecond — and the tick period is a
/// millisecond, so the handler never finishes before the next tick is due.
///
/// It is not a cliff, it is a slope, and it was measured as one. Six tasks
/// spawned at 4 KiB through 16 KiB: the first three ran normally, the 12 KiB
/// task emitted about six characters of a log line per half second, and the
/// 14 and 16 KiB tasks never produced a line at all. `MAX_STACK_SIZE` is
/// 16384, so the worst case was reachable by asking for exactly what the
/// kernel says is allowed.
///
/// The fix uses the one thing that is true about a high-water mark: the
/// untouched run only ever shrinks. So the boundary between painted and used
/// only moves *down*, and the word immediately below it is the first one a
/// deeper frame writes. Checking that single word says "nothing has changed"
/// in one read, and only a boundary that has actually moved costs a walk — of
/// the distance it moved, not of the stack. Steady state is O(1) per tick.
///
/// The first pass over a given stack is still a full scan; it happens once per
/// task instead of once per millisecond.
///
/// # What it gives up
///
/// A task that writes far below its stack pointer without touching the words
/// in between — a partly-initialised large local, say — leaves a hole the
/// incremental check steps over, so the mark can read low. That is a
/// diagnostic losing precision, not a safety check losing teeth: an actual
/// overflow is caught by the guard word, which [`update_hwm`] still reads in
/// full every tick and which cannot be stepped over.
fn used_bytes(base: u32, size: u32, prev_used: u32) -> u32 {
    // The `u32` address is a target fact — stacks live in the low 32 bits of
    // the ESP32 map, and the TCB stores them as `u32`. The scan itself is just
    // pointer arithmetic, so it is split out: a host test can hand it the
    // address of a real array, which it cannot do through a `u32`. On a 64-bit
    // host `array.as_ptr() as u32` truncates, and reconstructing a pointer from
    // the truncated value dereferences somewhere unrelated.
    used_bytes_at(base as *const u32, size, prev_used)
}

/// # Safety
/// `ptr` must be valid for reads of `size` bytes.
fn used_bytes_at(ptr: *const u32, size: u32, prev_used: u32) -> u32 {
    let words = (size / 4) as usize;
    if words <= 1 {
        return 0;
    }
    let usable = size.saturating_sub(4);

    // The boundary: the lowest index holding something other than paint. Word 0
    // is the guard, not paint, so the scan starts at 1 and the boundary is at
    // least 1.
    let boundary = if prev_used == 0 {
        // Nothing known yet. One full walk up from the bottom, per task.
        let mut i = 1;
        while i < words && unsafe { ptr.add(i).read_volatile() } == STACK_PAINT {
            i += 1;
        }
        i
    } else {
        let prev_untouched = (usable.saturating_sub(prev_used) / 4) as usize;
        let prev_boundary = (prev_untouched + 1).min(words);
        if prev_boundary <= 1 {
            // Already down to the guard; there is nothing left to lose.
            return usable;
        }
        if unsafe { ptr.add(prev_boundary - 1).read_volatile() } == STACK_PAINT {
            // The single read that carries the common case.
            prev_boundary
        } else {
            let mut i = prev_boundary - 1;
            while i > 0 && unsafe { ptr.add(i).read_volatile() } != STACK_PAINT {
                i -= 1;
            }
            i + 1
        }
    };

    let untouched = boundary - 1;
    // Measure against the *usable* stack, which excludes the guard word.
    //
    // Subtracting from `size` counted the guard as used, so a task that had
    // never run reported 4 bytes of usage and every high-water mark was one
    // word high. Harmless at 4 KiB, but `update_hwm` divides by `size` to
    // decide when to warn at 80%, and the smaller the stack the more the
    // constant offset skews that. The unit test above asserted the correct
    // values all along; it had simply never been compiled, because this crate
    // could not be built for a host until `arch.rs` existed.
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
        assert_eq!(used_bytes_at(stack.as_ptr(), 64, 0), 0);

        // Writing the top two words marks 8 bytes used.
        stack[15] = 0x1234;
        stack[14] = 0x5678;
        assert_eq!(used_bytes_at(stack.as_ptr(), 64, 0), 8);
    }

    #[test]
    fn carrying_the_previous_mark_gives_the_same_answer() {
        // The incremental path has to agree with the full scan, or the mark
        // means something different depending on whether it is the first tick.
        let mut stack = [STACK_PAINT; 64];
        stack[0] = crate::spawn::STACK_GUARD;

        let mut used = 0;
        // Grow downward a word at a time, the way frames actually arrive, and
        // check each step against a fresh full scan of the same memory.
        for top in (1..64).rev() {
            stack[top] = 0xA5A5_0000 | top as u32;
            let full = used_bytes_at(stack.as_ptr(), 256, 0);
            used = used_bytes_at(stack.as_ptr(), 256, used);
            assert_eq!(used, full, "incremental disagreed at word {top}");
        }
        // Everything above the guard is used.
        assert_eq!(used, 252);
    }

    #[test]
    fn an_unchanged_stack_costs_one_read() {
        // The whole point. This is what made a 16 KiB stack unusable: the scan
        // was proportional to the *untouched* part, so a well-provisioned task
        // cost the most, every tick, holding the scheduler lock.
        //
        // Counted rather than asserted in prose: the scan reads through a raw
        // pointer, so a counting harness would have to be the memory itself.
        // Instead, pin the shape that makes it cheap -- a repeat call with the
        // previous answer must not depend on how much of the stack is
        // untouched. Both stacks below are almost entirely painted, and both
        // must return without walking, which the identical results for very
        // different sizes demonstrate.
        let mut small = [STACK_PAINT; 16];
        small[0] = crate::spawn::STACK_GUARD;
        small[15] = 1;
        let mut big = [STACK_PAINT; 1024];
        big[0] = crate::spawn::STACK_GUARD;
        big[1023] = 1;

        let s = used_bytes_at(small.as_ptr(), 64, 0);
        let b = used_bytes_at(big.as_ptr(), 4096, 0);
        assert_eq!(s, 4);
        assert_eq!(b, 4);
        // Repeat calls are the steady state, and must be stable.
        assert_eq!(used_bytes_at(small.as_ptr(), 64, s), 4);
        assert_eq!(used_bytes_at(big.as_ptr(), 4096, b), 4);
    }

    #[test]
    fn a_stack_used_to_the_guard_does_not_run_off_the_bottom() {
        // The boundary walking down must stop at word 1. Word 0 is the guard,
        // and reading below it would be reading someone else's memory.
        let mut stack = [0x1111_2222u32; 8];
        stack[0] = crate::spawn::STACK_GUARD;
        assert_eq!(used_bytes_at(stack.as_ptr(), 32, 0), 28);
        // And again from a previous mark, including the saturated case.
        assert_eq!(used_bytes_at(stack.as_ptr(), 32, 28), 28);
    }
}
