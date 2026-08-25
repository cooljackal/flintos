// SPDX-License-Identifier: Apache-2.0

//! Task creation and stack allocation (plan W1.3, W0.1).
//!
//! Stacks are bump-allocated from the linker-defined `task_stacks` region
//! (`_task_stack_start`/`_task_stack_end`) — no magic addresses. Each task's
//! initial `TaskContext` is built so the first dispatch (`rfe` in the trap
//! handler) lands at the entry point with a clean single register window.

use hal::arch::Architecture;
use hal::types::{Priority, TaskId};

use crate::scheduler::{self, TaskState};

const MAX_STACK_SIZE: u32 = 16384;

/// Smallest stack a task may request.
///
/// The floor is not arbitrary. A trap lands on the *interrupted task's* stack
/// and immediately reserves a 288-byte-aligned frame, then calls `_flint_trap`,
/// whose own prologue reserves several hundred bytes more, and window overflows
/// spill further frames below that. A stack under this size can be overrun by
/// the interrupt machinery alone, before the task's own locals are counted.
const MIN_STACK_SIZE: u32 = 1024;

const STACK_PAINT: u32 = 0xDEADBEEF;

/// Written at the lowest address of every task stack and checked on each
/// context switch. Stack growth is downward, so this word is the first thing an
/// overflow destroys.
pub const STACK_GUARD: u32 = 0xFEEDFACE;

extern "C" {
    static _task_stack_start: u32;
    static _task_stack_end: u32;
}

/// Bump allocator offset into the task-stack region.
///
/// A `static mut`, and safe because its only reader/writer is
/// `allocate_stack`, which is called from inside `scheduler::with` — the
/// allocation has to be atomic with the TCB slot claim anyway, so the
/// scheduler lock is the right one and a second lock here would be wrong.
/// Calling `allocate_stack` from outside that lock would hand two cores the
/// same stack.
static mut STACK_ALLOC_OFFSET: u32 = 0;

pub(crate) fn paint_stack(base: u32, size: u32) {
    let words = (size / 4) as usize;
    let ptr = base as *mut u32;
    for i in 0..words {
        unsafe { ptr.add(i).write(STACK_PAINT) };
    }
    // The guard replaces the paint word at the lowest address, so an overflow
    // is distinguishable from ordinary high-water-mark consumption.
    unsafe { ptr.write(STACK_GUARD) };
}

/// True if `tcb`'s stack guard is still intact.
///
/// A false return means the task has already written past the bottom of its
/// stack and has corrupted whatever lies below. Tasks with `stack_size == 0`
/// (the idle task, which runs on the boot stack) are always reported intact.
pub fn stack_guard_intact(stack_base: u32, stack_size: u32) -> bool {
    if stack_size == 0 {
        return true;
    }
    unsafe { (stack_base as *const u32).read_volatile() == STACK_GUARD }
}

pub(crate) fn allocate_stack(size: u32) -> Option<u32> {
    unsafe {
        let region_start = core::ptr::addr_of!(_task_stack_start) as u32;
        let region_end = core::ptr::addr_of!(_task_stack_end) as u32;

        // Align the base as well as the size. Aligning only the size leaves
        // every stack's alignment hostage to wherever the linker happened to
        // place the region, and Xtensa requires a 16-byte-aligned SP.
        let base = (region_start.checked_add(STACK_ALLOC_OFFSET)?).checked_add(15)? & !15;
        let size = size.checked_add(15)? & !15;
        let end = base.checked_add(size)?;
        if end > region_end {
            return None; // pool exhausted (W3.5: surfaced, not panicked)
        }
        STACK_ALLOC_OFFSET = end - region_start;
        Some(base)
    }
}

/// Kernel-internal spawn. Returns `None` if no TCB slot or stack is available.
pub fn sys_spawn(
    name: &'static str,
    entry: fn(),
    priority: Priority,
    stack_size: usize,
) -> Option<TaskId> {
    sys_spawn_with_affinity(name, entry, priority, stack_size, scheduler::Affinity::Any)
}

/// Spawn with an explicit core affinity.
///
/// `Affinity::Any` is what `sys_spawn` passes and what most tasks want.
/// Pinning is for a task that cannot float: a driver whose peripheral
/// interrupt is routed to one core's matrix, or work with a timing budget that
/// a migration would blow.
pub fn sys_spawn_with_affinity(
    name: &'static str,
    entry: fn(),
    priority: Priority,
    stack_size: usize,
    affinity: scheduler::Affinity,
) -> Option<TaskId> {
    spawn_inner(
        name,
        entry,
        priority,
        stack_size,
        affinity,
        StackSource::Pool,
    )
}

/// Where a new task's stack comes from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StackSource {
    /// The linker's `task_stacks` region, bump-allocated and never reclaimed.
    /// What every statically created task uses.
    Pool,
    /// The radio heap, returned when the task is deleted. For runtime-created
    /// tasks only — see `kernel::dynobj`.
    Heap,
}

/// Spawn with an explicit stack source.
///
/// # Safety of the `Heap` variant
/// The caller must delete the task through `dynobj::delete_task`, which is
/// what returns the stack. Letting a heap-backed task simply run off the end
/// of its entry point leaks the stack exactly as the pool would.
pub fn sys_spawn_from(
    name: &'static str,
    entry: fn(),
    priority: Priority,
    stack_size: usize,
    affinity: scheduler::Affinity,
    stack: StackSource,
) -> Option<TaskId> {
    spawn_inner(name, entry, priority, stack_size, affinity, stack)
}

fn spawn_inner(
    name: &'static str,
    entry: fn(),
    priority: Priority,
    stack_size: usize,
    affinity: scheduler::Affinity,
    stack: StackSource,
) -> Option<TaskId> {
    // Validate before taking a TCB slot, so a rejected request leaves no trace.
    //
    // These used to be silently clamped with `.min(MAX_STACK_SIZE)`. With no
    // MPU, a task that believes it has the stack it asked for and quietly got
    // less overflows into its neighbour -- exactly the silent-wrong-answer
    // failure the mutex, queue, and timer paths were all changed to reject.
    let stack_size = stack_size as u32;
    if !(MIN_STACK_SIZE..=MAX_STACK_SIZE).contains(&stack_size) {
        return None;
    }

    scheduler::with(|sched| {
        let id = sched.alloc_id()? as usize;
        if let Some(tcb) = &mut sched.tasks[id] {
            tcb.affinity = affinity;
        }

        let stack_base = match stack {
            StackSource::Pool => allocate_stack(stack_size),
            // Sixteen-byte aligned: the windowed ABI spills in sixteen-byte
            // units and `init_context` puts the initial frame at the top.
            StackSource::Heap => {
                let p = unsafe { crate::heap::alloc(stack_size as usize, 16) };
                // `stack_base` is a `u32` because the target's address space
                // is. Truncating a wider pointer would paint the guard, and
                // then run the task, at an address that is not the stack --
                // which is a segfault on a 64-bit host and silent corruption
                // anywhere. Refuse instead, and hand the memory straight back.
                let addr = p as usize;
                if p.is_null() {
                    None
                } else if addr > u32::MAX as usize {
                    unsafe { crate::heap::free(p, crate::heap::Caps::Internal) };
                    None
                } else {
                    Some(addr as u32)
                }
            }
        };
        let stack_base = match stack_base {
            Some(b) => b,
            None => {
                // Roll back the TCB slot we just took.
                sched.tasks[id] = None;
                return None;
            }
        };
        paint_stack(stack_base, stack_size);

        if let Some(tcb) = &mut sched.tasks[id] {
            tcb.init_common(name, entry, priority.numeric(), TaskState::Ready);
            tcb.stack_base = stack_base;
            tcb.stack_size = stack_size;
            tcb.heap_stack = stack == StackSource::Heap;
            tcb.stack_hwm = 0;
            unsafe {
                crate::arch::SelectedArch::init_context(
                    &mut tcb.context,
                    entry as usize,
                    stack_base + stack_size,
                )
            };
        }
        // Publish only after the context is complete. A ready bit alone does
        // not wake a remote idle core: make_ready also queues its reschedule
        // notification for delivery after the scheduler lock is released.
        sched.make_ready(id as u32);

        Some(TaskId(id as u32))
    })
}

/// Called when a task function returns. De-schedules the task forever.
#[no_mangle]
pub(crate) extern "C" fn flint_task_exit() -> ! {
    scheduler::with(|sched| {
        let cur = sched.current();
        let Some(tcb) = &mut sched.tasks[cur as usize] else {
            return;
        };
        tcb.state = TaskState::Suspended;
        let prio = tcb.priority;
        // Recompute rather than clear. A priority level is shared: clearing the
        // bit outright strands every *other* Ready task at the same level,
        // because `schedule()` only visits levels present in ready_mask. Two
        // round-robin peers, one returns from its entry function, and the
        // survivor is never dispatched again -- its TCB still says Ready, so
        // nothing anywhere reports a problem.
        sched.recompute_ready_bit(prio);
    });
    scheduler::request_switch();
    // Wait to be switched away.
    loop {
        crate::arch::wait_for_interrupt();
    }
}
