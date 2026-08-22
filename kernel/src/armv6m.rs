// SPDX-License-Identifier: Apache-2.0

//! Cortex-M trap-to-scheduler bridge.

use crate::arch::{Context, Tick};
use crate::scheduler::{self, TaskState};
use hal::tick::TickSource;

#[no_mangle]
extern "C" fn _flint_armv6m_start_tick() {
    unsafe { Tick::start() };
}

#[no_mangle]
extern "C" fn _flint_armv6m_task_exit() -> ! {
    crate::spawn::flint_task_exit()
}

#[no_mangle]
extern "C" fn _flint_armv6m_first_stack() -> u32 {
    scheduler::with(|sched| {
        let next = sched.schedule();
        assert!(next != u32::MAX, "ARM first launch needs one ready task");
        sched.set_current(next);
        sched.tasks[next as usize]
            .as_ref()
            .expect("scheduled task has a TCB")
            .context
            .stack_pointer
    })
}

#[no_mangle]
extern "C" fn _flint_armv6m_systick() {
    Tick::tick();
    let now = Tick::now();
    crate::timer::process_timers(now);
    let switch = scheduler::with(|sched| sched.on_tick(now));
    if switch {
        // A flag alone is insufficient on Cortex-M: PendSV must be made
        // pending so the switch runs after this SysTick handler returns.
        scheduler::request_switch();
    }
}

#[no_mangle]
extern "C" fn _flint_armv6m_switch(stack_pointer: u32) -> u32 {
    scheduler::with(|sched| {
        let current = sched.current();
        if !scheduler::take_pending_switch() {
            return stack_pointer;
        }
        let next = sched.schedule();
        if next == current {
            return stack_pointer;
        }
        if current != u32::MAX {
            if let Some(tcb) = &mut sched.tasks[current as usize] {
                tcb.context = Context { stack_pointer };
                if tcb.state == TaskState::Running {
                    tcb.state = TaskState::Ready;
                    sched.ready_mask |= 1u64 << tcb.priority;
                }
            }
        }
        sched.set_current(next);
        sched.tasks[next as usize]
            .as_ref()
            .expect("scheduled task has a TCB")
            .context
            .stack_pointer
    })
}
