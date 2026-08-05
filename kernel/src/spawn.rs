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
    // Kernel mode, not user mode.
    //
    // Flint runs in a single protection domain by design -- there is no MPU and
    // no privilege separation -- and startup.S runs the kernel itself with
    // PS.UM clear. Giving tasks PS.UM=1 bought nothing, made them the only code
    // in the system running in user mode, and routed their exceptions to a
    // different vector (0x340 rather than 0x300) than everything else.
    //
    // It also matters for the window handlers: `s32e` and `l32e`, which the
    // overflow and underflow vectors are built from, are privileged in some
    // Xtensa configurations. Running tasks at the same level as the rest of the
    // kernel removes that as a variable.
    ctx.ps = PS_WOE | (1 << PS_CALLINC_SHIFT);
    ctx.sar = 0;
    ctx.lbeg = 0;
    ctx.lend = 0;
    ctx.lcount = 0;
    // Reserve a base save area below the top of the stack. The task's first
    // instruction is `entry`, and if that raises a window overflow the handler
    // spills through `s32e a0, a5, -16`, writing *below* the base register it
    // is handed. Without a reserved area those stores land outside the stack.
    let sp = (stack_top - BASE_SAVE_AREA) & !15;

    ctx.a = [0u32; 16];
    // a0 = 0, matching the reference Xtensa ports. A zero return address
    // terminates the register-window spill chain: the overflow handler walks
    // callers via a0, and a real address here presents the outermost frame as
    // having a caller to spill into, which it does not.
    //
    // The cost is that a task returning from its entry function jumps to 0
    // rather than reaching task_exit. That path is already unreachable for the
    // demo tasks, which loop forever, and a crash there is easier to diagnose
    // than a task that will not start.
    ctx.a[0] = 0;
    ctx.a[1] = sp;

    // Every window's a1 slot gets a valid stack pointer.
    //
    // This is what was wrong: the register file was zeroed, so whichever
    // physical register the overflow handler used as its spill base held 0 and
    // the spill addressed 0xFFFFFFF0. Observed on hardware as a task frozen on
    // its own `entry` instruction -- PC, SP and WINDOWSTART identical across
    // tens of thousands of ticks, with the stack completely untouched -- while
    // the kernel ticked normally around it.
    //
    // Xtensa windows overlap by four registers, so window WB+k begins at
    // physical register 4k and its a1 is physical register 4k+1. The stack
    // pointers therefore live at every fourth register across the whole file,
    // starting at index 1 -- a1, a5, a9, a13, then onward through ar_rest.
    //
    // Note a5 in particular: it is window WB+1's a1, which is the base the very
    // first overflow uses. An earlier attempt set only ar_rest[1], [17] and
    // [33], which are real a1 slots but belong to windows WB+4, WB+8 and WB+12
    // -- so the first spill still had a zero base and nothing changed.
    // Give every window in the current frame a valid stack pointer. Xtensa
    // windows overlap by four registers, so window WB+k's a1 is register 4k+1:
    // a1, a5, a9, a13.
    let mut i = 1;
    while i < 16 {
        ctx.a[i] = sp;
        i += 4;
    }

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
