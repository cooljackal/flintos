// SPDX-License-Identifier: Apache-2.0

//! Dynamic-object self-tests. Included by [`crate::selftest`].
//!
//! Most of `dynobj` is tested on the host, where the allocator is backed by an
//! ordinary buffer. Two things are not testable there and are the reason this
//! file exists.
//!
//! **Task create and delete needs 32-bit addresses.** A task's `stack_base` is
//! a `u32`, because the target's address space is. On a 64-bit host the heap
//! sits above 4 GiB, so `spawn_task` refuses rather than truncating — correct,
//! but it means the create/delete cycle, and the no-leak property the issue
//! actually asks for, can only be exercised on the chip.
//!
//! **The heap is real memory here.** The host tests prove the bookkeeping
//! balances; these prove the stack a task is handed is memory it can use.

use hal::types::Priority;

use crate::dynobj;
use crate::heap::{self, Caps};

use super::Check;

/// A task that returns immediately. Never actually scheduled by these tests —
/// they create and delete without letting it run, because what is under test
/// is the lifecycle bookkeeping, not the entry point.
fn noop() {}

/// A dynamic task's stack must come from the heap and go back to it.
///
/// The leak this guards is the whole reason dynamic tasks do not use the
/// linker's pool: that pool is a bump allocator and has nothing to give back
/// to, so a blob creating and deleting tasks all session would exhaust it.
pub(super) fn a_dynamic_task_returns_its_stack() -> Check {
    let before = heap::free_bytes(Caps::Internal);
    let id = match dynobj::spawn_task("selftest-dyn", noop, Priority::Normal(3), 4096) {
        Some(id) => id,
        None => return Err("could not create a dynamic task"),
    };
    if heap::free_bytes(Caps::Internal) >= before {
        // Delete first, or the failure leaks on top of itself.
        let _ = dynobj::delete_task(id);
        return Err("creating a task consumed no heap");
    }
    if dynobj::delete_task(id).is_err() {
        return Err("deleting a freshly created task was refused");
    }
    if heap::free_bytes(Caps::Internal) != before {
        return Err("deleting a task did not return its whole stack");
    }
    Ok(())
}

/// Repeated create and delete must not lose memory.
///
/// One cycle balancing can hide an off-by-a-header; twenty cannot. This is the
/// shape of what the radio actually does.
pub(super) fn task_churn_does_not_leak() -> Check {
    let before = heap::free_bytes(Caps::Internal);
    for _ in 0..20 {
        let id = match dynobj::spawn_task("churn", noop, Priority::Normal(3), 2048) {
            Some(id) => id,
            None => return Err("create failed partway through the churn"),
        };
        if dynobj::delete_task(id).is_err() {
            return Err("delete failed partway through the churn");
        }
    }
    if heap::free_bytes(Caps::Internal) != before {
        return Err("create/delete churn leaked heap");
    }
    Ok(())
}

/// The reaper must return a self-deleted task's stack.
///
/// This is the half of self-delete that can be checked from a test. The other
/// half — `delete_self` itself — never returns by design, so calling it here
/// would end the self-test rather than report on it. What is set up instead is
/// exactly the state it leaves behind: a `Deleting` task, with a real heap
/// stack, that no core is running.
///
/// The stack is a genuine 32-bit heap address, which is why this cannot live
/// in the host suite: there `stack_base` cannot hold a heap pointer at all.
pub(super) fn the_reaper_returns_a_deleted_task_s_stack() -> Check {
    let before = heap::free_bytes(Caps::Internal);

    let stack = unsafe { heap::alloc(2048, 16) };
    if stack.is_null() {
        return Err("could not allocate a stack to hand the reaper");
    }
    let id = crate::scheduler::with(|sched| {
        let id = match sched.alloc_id() {
            Some(id) => id,
            None => return None,
        };
        if let Some(tcb) = &mut sched.tasks[id as usize] {
            tcb.state = crate::scheduler::TaskState::Deleting;
            tcb.heap_stack = true;
            tcb.stack_base = stack as u32;
            tcb.stack_size = 2048;
        }
        Some(id)
    });
    let id = match id {
        Some(id) => id,
        None => {
            unsafe { heap::free(stack, Caps::Internal) };
            return Err("no free TCB slot");
        }
    };

    if dynobj::reap_deleted() == 0 {
        return Err("the reaper passed over a deleted task");
    }
    if crate::scheduler::with(|s| s.tasks[id as usize].is_some()) {
        return Err("the reaper did not free the slot");
    }
    if heap::free_bytes(Caps::Internal) != before {
        return Err("the reaper did not return the stack");
    }
    Ok(())
}

/// A task another core is running must not be reaped.
///
/// The check that makes self-delete safe at all: until the dying task stops
/// being some core's current, its stack is still in use.
pub(super) fn the_reaper_skips_a_task_a_core_is_on() -> Check {
    // The caller is, by definition, this core's current task. Marking it
    // Deleting is precisely what `delete_self` does before the switch lands.
    let me = dynobj::current_task();
    let restore = crate::scheduler::with(|sched| {
        sched.tasks[me as usize].as_mut().map(|tcb| {
            let was = tcb.state;
            tcb.state = crate::scheduler::TaskState::Deleting;
            was
        })
    });
    if restore.is_none() {
        return Err("the running task has no control block");
    }
    let reaped = dynobj::reap_deleted();
    let survived = crate::scheduler::with(|s| s.tasks[me as usize].is_some());
    crate::scheduler::with(|sched| {
        if let (Some(tcb), Some(was)) = (sched.tasks[me as usize].as_mut(), restore) {
            tcb.state = was;
        }
    });
    if reaped != 0 || !survived {
        return Err("the reaper freed a task this core is running on");
    }
    Ok(())
}

/// A dynamic queue must carry bytes through real memory.
pub(super) fn a_dynamic_queue_round_trips_on_hardware() -> Check {
    let before = heap::free_bytes(Caps::Internal);
    let mut q = match dynobj::DynQueue::create(8, 4) {
        Some(q) => q,
        None => return Err("could not create a queue"),
    };
    for i in 0..8u32 {
        if !unsafe { q.try_send(&i as *const u32 as *const u8) } {
            q.delete();
            return Err("a queue with room refused an item");
        }
    }
    if !q.is_full() {
        q.delete();
        return Err("eight items into an eight-slot queue is not full");
    }
    for i in 0..8u32 {
        let mut got = 0u32;
        if !unsafe { q.try_recv(&mut got as *mut u32 as *mut u8) } {
            q.delete();
            return Err("a queue with items refused to yield one");
        }
        if got != i {
            q.delete();
            return Err("a queue returned items out of order");
        }
    }
    q.delete();
    if heap::free_bytes(Caps::Internal) != before {
        return Err("deleting a queue did not return its storage");
    }
    Ok(())
}

/// Semaphores and event groups, briefly, on the chip.
///
/// The logic is host-tested; what this adds is that the atomics behave on
/// Xtensa, where `fetch_or` compiles to something rather different.
pub(super) fn semaphores_and_event_bits_work_on_target() -> Check {
    let mut s = match dynobj::Semaphore::create(2, 0) {
        Some(s) => s,
        None => return Err("could not create a semaphore"),
    };
    if s.try_take() {
        return Err("an empty semaphore handed out a permit");
    }
    if !s.give() || !s.give() {
        return Err("a semaphore refused a permit below its maximum");
    }
    if s.give() {
        return Err("a semaphore exceeded its maximum");
    }
    if !s.try_take() || !s.try_take() || s.try_take() {
        return Err("semaphore count did not match what was given");
    }

    let g = dynobj::EventGroup::new();
    g.set(0b1010);
    if g.get() != 0b1010 {
        return Err("event bits did not read back");
    }
    // Already satisfied, so this must not block even with a zero timeout.
    if g.wait(0b0010, false, true, 0) != Some(0b1010) {
        return Err("a satisfied wait did not return immediately");
    }
    if g.get() != 0b1000 {
        return Err("clear-on-exit consumed the wrong bits");
    }
    Ok(())
}
