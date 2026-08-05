// SPDX-License-Identifier: Apache-2.0

//! Task creation and stack allocation (plan W1.3, W0.1).
//!
//! Stacks are bump-allocated from the linker-defined `task_stacks` region
//! (`_task_stack_start`/`_task_stack_end`) — no magic addresses. Each task's
//! initial `TaskContext` is built so the first dispatch (`rfe` in the trap
//! handler) lands at the entry point with a clean single register window.

use flint_hal::types::{Priority, TaskId};
use flint_arch_xtensa::registers::{PS_UM, PS_WOE, PS_CALLINC_SHIFT};

use crate::scheduler::{self, TaskState};

const MAX_STACK_SIZE: u32 = 16384;
const STACK_PAINT: u32 = 0xDEADBEEF;

extern "C" {
    static _task_stack_start: u32;
    static _task_stack_end: u32;
}

/// Bump allocator offset into the task-stack region.
static mut STACK_ALLOC_OFFSET: u32 = 0;

fn paint_stack(base: u32, size: u32) {
    let words = (size / 4) as usize;
    let ptr = base as *mut u32;
    for i in 0..words {
        unsafe { ptr.add(i).write(STACK_PAINT) };
    }
}

fn allocate_stack(size: u32) -> Option<u32> {
    unsafe {
        let region_start = core::ptr::addr_of!(_task_stack_start) as u32;
        let region_end = core::ptr::addr_of!(_task_stack_end) as u32;
        let base = region_start + STACK_ALLOC_OFFSET;
        // 16-byte align each stack.
        let size = (size + 15) & !15;
        if base + size > region_end {
            return None; // pool exhausted (W3.5: surfaced, not panicked)
        }
        STACK_ALLOC_OFFSET += size;
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
    scheduler::with(|sched| {
        let id = sched.alloc_id()? as usize;

        let stack_size = (stack_size as u32).min(MAX_STACK_SIZE);
        let stack_base = match allocate_stack(stack_size) {
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
/// HARDWARE-UNVERIFIED window setup: `windowstart = 1`, `windowbase = 0` give a
/// single live frame; `PS.CALLINC = 1` matches a `call4`-style entry so the
/// function's `entry` prologue rotates correctly. `a[0]` points at the task-exit
/// trampoline so a task that returns is cleanly de-scheduled rather than
/// jumping to garbage (plan W1.3). These values need on-target tuning at G1.
unsafe fn init_context(ctx: &mut flint_hal::TaskContext, entry: usize, stack_top: u32) {
    ctx.pc = entry as u32;
    ctx.ps = PS_UM | PS_WOE | (1 << PS_CALLINC_SHIFT);
    ctx.sar = 0;
    ctx.lbeg = 0;
    ctx.lend = 0;
    ctx.lcount = 0;
    ctx.a = [0u32; 16];
    ctx.a[0] = task_exit as usize as u32; // return address → task_exit
    ctx.a[1] = stack_top & !15; // SP, 16-aligned
    ctx.windowbase = 0;
    ctx.windowstart = 1;
}

/// Called when a task function returns. De-schedules the task forever.
extern "C" fn task_exit() -> ! {
    scheduler::with(|sched| {
        let cur = sched.current;
        if let Some(tcb) = &mut sched.tasks[cur as usize] {
            tcb.state = TaskState::Suspended;
            let prio = tcb.priority;
            sched.ready_mask &= !(1u64 << prio);
        }
    });
    scheduler::request_switch();
    // Wait to be switched away.
    loop {
        unsafe { core::arch::asm!("waiti 0") };
    }
}
