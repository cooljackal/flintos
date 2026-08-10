// SPDX-License-Identifier: Apache-2.0

//! Task creation and stack allocation (plan W1.3, W0.1).
//!
//! Stacks are bump-allocated from the linker-defined `task_stacks` region
//! (`_task_stack_start`/`_task_stack_end`) — no magic addresses. Each task's
//! initial `TaskContext` is built so the first dispatch (`rfe` in the trap
//! handler) lands at the entry point with a clean single register window.

use hal::types::{Priority, TaskId};
use crate::arch::registers::PS_WOE;

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

/// Bytes reserved at the top of a task stack for the caller's register save
/// area. Xtensa window overflow handlers store four to twelve registers just
/// below the base register they are given, so the outermost frame needs real
/// memory there or the very first spill writes off the end of the stack.
const BASE_SAVE_AREA: u32 = 32;

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

fn paint_stack(base: u32, size: u32) {
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

fn allocate_stack(size: u32) -> Option<u32> {
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
    spawn_inner(name, entry, priority, stack_size, affinity, StackSource::Pool)
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
            tcb.name = name;
            tcb.entry = Some(entry);
            tcb.base_prio = priority.numeric();
            tcb.priority = priority.numeric();
            tcb.stack_base = stack_base;
            tcb.stack_size = stack_size;
            tcb.heap_stack = stack == StackSource::Heap;
            tcb.stack_hwm = 0;
            tcb.state = TaskState::Ready;
            tcb.quantum = scheduler::DEFAULT_QUANTUM_MS;
            unsafe { init_context(&mut tcb.context, entry as usize, stack_base + stack_size) };
            let prio = tcb.priority;
            sched.ready_mask |= 1u64 << prio;
        }

        Some(TaskId(id as u32))
    })
}

/// Initialise a fresh task's saved context.
///
/// The task is not entered directly. `pc` points at `_flint_task_start`, an
/// assembly trampoline with no `entry` of its own, which reaches the task
/// through a real `callx4` so the hardware -- not this function -- establishes
/// CALLINC, the return address and the window state.
///
/// Hand-synthesising that state is what failed on hardware: a real `call4`
/// leaves the return address in the caller's a4, which only becomes the
/// callee's a0 once `entry` rotates the window. Putting it in a0 here produced
/// a layout the hardware never generates, and the task's first `entry` never
/// retired.
unsafe fn init_context(ctx: &mut hal::TaskContext, entry: usize, stack_top: u32) {
    extern "C" {
        fn _flint_task_start();
    }

    ctx.pc = _flint_task_start as *const () as usize as u32;

    // Kernel mode: FlintOS is a single protection domain and startup.S runs the
    // kernel with PS.UM clear, so tasks run at the same level as the handlers
    // that serve them. CALLINC is left at 0 -- the trampoline's `callx4` sets
    // it, rather than this function pretending a call already happened.
    ctx.ps = PS_WOE;
    ctx.sar = 0;
    ctx.lbeg = 0;
    ctx.lend = 0;
    ctx.lcount = 0;

    // Reserve a save area below the top of the stack so the first window
    // overflow has real memory to spill into.
    let sp = (stack_top - BASE_SAVE_AREA) & !15;

    ctx.a = [0u32; 16];
    ctx.a[0] = 0;       // no caller; terminates the spill chain
    ctx.a[1] = sp;
    ctx.a[3] = entry as u32; // the trampoline calls this

    // Xtensa windows overlap by four registers, so window WB+k's a1 is register
    // 4k+1. Give each a valid stack pointer so any spill base is sane.
    ctx.a[5] = sp;
    ctx.a[9] = sp;
    ctx.a[13] = sp;

    ctx.windowbase = 0;
    ctx.windowstart = 1;
}

/// Called when a task function returns. De-schedules the task forever.
#[no_mangle]
extern "C" fn flint_task_exit() -> ! {
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
