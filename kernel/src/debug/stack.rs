// SPDX-License-Identifier: Apache-2.0

//! Stack high-water marking and overflow detection.
//!
//! Every task stack is painted with `STACK_PAINT` at spawn, except its lowest
//! word which holds `STACK_GUARD`. Stacks grow downward, so untouched memory
//! accumulates at the *low* end and the guard is the first thing an overflow
//! destroys.

use crate::scheduler;
use crate::spawn::{stack_guard_intact, STACK_GUARD};

const STACK_PAINT: u32 = 0xDEADBEEF;

/// Fraction of the stack above which a warning is emitted, in percent.
const WARN_PCT: u32 = 80;

/// Update the high-water mark for a task and check its guard word.
///
/// # Safety contract
/// Reads and writes the global scheduler directly rather than through
/// `scheduler::with`. This is only sound because the sole caller is the trap
/// handler, which runs with interrupts already masked.
pub fn update_hwm(task_id: u32) {
    let sched = scheduler::global();
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
    let words = (size / 4) as usize;
    let ptr = base as *const u32;
    let mut untouched = 0usize;
    // Word 0 is the guard, not paint, so start the scan above it.
    for i in 1..words {
        let w = unsafe { ptr.add(i).read_volatile() };
        if w != STACK_PAINT {
            break;
        }
        untouched += 1;
    }
    size.saturating_sub((untouched * 4) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_is_distinct_from_paint() {
        // If these ever collided, an overflow that happened to write the paint
        // value would look like an intact guard.
        assert_ne!(STACK_GUARD, STACK_PAINT);
    }

    #[test]
    fn used_bytes_counts_from_the_top_down() {
        // A stack fully painted except the guard is entirely unused.
        let mut stack = [STACK_PAINT; 16];
        stack[0] = STACK_GUARD;
        let base = stack.as_ptr() as u32;
        assert_eq!(used_bytes(base, 64), 0);

        // Writing the top two words marks 8 bytes used.
        stack[15] = 0x1234;
        stack[14] = 0x5678;
        assert_eq!(used_bytes(base, 64), 8);
    }
}
