// SPDX-License-Identifier: Apache-2.0

//! Trap handler — the single point where context switches happen (plan W1.2).
//!
//! `_flint_trap` is called by the assembly trap entry (`vectors.S`) with a
//! pointer to a `TaskContext`-shaped frame holding the interrupted state. It
//! services the timer tick, the software-interrupt switch request, and routed
//! peripheral IRQs, then returns the `TaskContext` to resume — the same frame
//! for "no switch", or the next task's saved context for a switch.
//!
//! Interrupts are masked for the whole handler (we are inside the trap), so the
//! scheduler is accessed directly without an extra critical section.

use flint_hal::tick::TickSource;
use flint_hal::types::TaskContext;
use flint_arch_xtensa::registers;
use flint_arch_xtensa::tick::XtensaTick;
use core::sync::atomic::Ordering;

use crate::scheduler::{self, TaskState};
use crate::{interrupt, timer, debug};

/// Called from `_flint_trap_entry` in vectors.S.
///
/// # Safety
/// `frame` must point to a valid 96-byte `TaskContext`-layout frame built by
/// the trap entry stub.
#[no_mangle]
pub extern "C" fn _flint_trap(frame: *mut TaskContext) -> *mut TaskContext {
    let cause = unsafe { registers::read_exccause() };

    if cause == registers::EXCCAUSE_LEVEL1_INTERRUPT {
        let pending = unsafe { registers::read_interrupt() & registers::read_intenable() };

        // Timer tick.
        if pending & registers::INT_TIMER0_MASK != 0 {
            XtensaTick::tick(); // ack + re-arm + advance counter
            let now = XtensaTick::now();
            if scheduler::global().on_tick(now) {
                scheduler::set_pending_switch();
            }
            timer::process_timers(now);
            // Stack high-water update for the running task.
            let cur = scheduler::global().current;
            debug::stack::update_hwm(cur);
        }

        // Software interrupt: a cooperative switch was requested.
        if pending & registers::INT_SOFTWARE_MASK != 0 {
            unsafe { registers::intclear(registers::INT_SOFTWARE_MASK) };
        }

        // Routed peripheral IRQs (everything except timer/software).
        let routed = pending & !(registers::INT_TIMER0_MASK | registers::INT_SOFTWARE_MASK);
        if routed != 0 {
            for irq in 0..32u32 {
                if routed & (1 << irq) != 0 {
                    interrupt::dispatch(irq as u8);
                }
            }
        }
    } else {
        // A genuine exception (not an interrupt) reached the trap handler.
        // In a single protection domain this is a fatal fault — dump it over
        // raw UART0 (works even before our own UART init) and halt.
        let (epc, ps, vaddr) = unsafe {
            ((*frame).pc, (*frame).ps, registers::read_excvaddr())
        };
        debug::fault::raw_uart_fault("exc", cause, epc, ps, vaddr);
    }

    // Decide whether to switch.
    let sched = scheduler::global();
    if scheduler::take_pending_switch() {
        let cur = sched.current;
        let next = sched.schedule();
        if next != cur {
            // Save the interrupted context into the current task's TCB, unless
            // it has been torn down.
            if let Some(tcb) = &mut sched.tasks[cur as usize] {
                // The current task was Running; demote to Ready unless it
                // blocked itself (block_current already set a blocked state).
                if tcb.state == TaskState::Running {
                    tcb.state = TaskState::Ready;
                    let prio = tcb.priority;
                    sched.ready_mask |= 1u64 << prio;
                }
                unsafe { core::ptr::copy_nonoverlapping(frame, &mut tcb.context, 1) };
            }
            sched.set_current(next);
            if let Some(tcb) = &mut sched.tasks[next as usize] {
                return &mut tcb.context as *mut TaskContext;
            }
        }
    }
    frame
}
