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
        let current = sched.current();
        if current != u32::MAX {
            return sched.tasks[current as usize]
                .as_ref()
                .expect("current task has no TCB")
                .context
                .stack_pointer;
        }
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
extern "C" fn _flint_armv6m_core1_boot() {
    assert_eq!(crate::smp::current_core().0, 1, "core-1 hook ran on core 0");
    let idle = crate::boot::CORE1_IDLE.load(portable_atomic::Ordering::Acquire);
    assert_ne!(idle, u32::MAX, "core-1 idle was not prepared");
    scheduler::with(|sched| {
        let tcb = sched.tasks[idle as usize]
            .as_mut()
            .expect("core-1 idle TCB disappeared");
        tcb.state = TaskState::Running;
    });
    scheduler::join_current_core(idle);
    enable_sio_fifo_irq(soc_rp2040::multicore::IRQ_SIO_PROC1);
}

#[inline]
pub(crate) fn enable_sio_fifo_irq(irq: u8) {
    const NVIC_ICPR: *mut u32 = 0xe000_e280 as *mut u32;
    const NVIC_ISER: *mut u32 = 0xe000_e100 as *mut u32;
    let mask = 1u32 << irq;
    unsafe {
        NVIC_ICPR.write_volatile(mask);
        NVIC_ISER.write_volatile(mask);
    }
}

#[no_mangle]
extern "C" fn _flint_armv6m_sio_irq(irq: u32) {
    let expected = if crate::smp::current_core().is_boot() {
        soc_rp2040::multicore::IRQ_SIO_PROC0
    } else {
        soc_rp2040::multicore::IRQ_SIO_PROC1
    };
    assert_eq!(irq as u8, expected, "SIO FIFO IRQ reached the wrong core");
    while soc_rp2040::multicore::fifo_try_pop().is_some() {}
    soc_rp2040::multicore::fifo_clear_errors();
    scheduler::request_switch();
}

#[no_mangle]
extern "C" fn _flint_armv6m_external_irq(irq: u32) {
    if irq < 32 {
        crate::interrupt::dispatch(irq as u8);
    }
}

#[no_mangle]
extern "C" fn _flint_armv6m_request_reschedule(core: u32) -> bool {
    if core >= u32::from(crate::smp::cores()) || core == u32::from(crate::smp::current_core().0) {
        return false;
    }
    const RESCHEDULE_TOKEN: u32 = 0x4652_5343;
    soc_rp2040::multicore::fifo_try_push(RESCHEDULE_TOKEN).is_ok()
}

#[no_mangle]
extern "C" fn _flint_armv6m_systick() {
    // Every core needs a local SysTick for preemption, but shared wall time
    // has exactly one writer or it runs twice as fast after core 1 joins.
    let switch = if crate::smp::is_timekeeper() {
        Tick::tick();
        let now = Tick::now();
        crate::timer::process_timers(now);
        scheduler::with(|sched| sched.on_tick(now)) || scheduler::switch_pending()
    } else {
        // Timeout ownership stays on core 0. Letting both cores consume the
        // same wakeup can make core 1 mark a core-0 task Ready, after which
        // core 0 no longer observes the transition that should preempt idle.
        // Wakeups normally carry an affinity-aware FIFO IPI. If the FIFO was
        // full, the published pending flag is the fallback that guarantees
        // the next local tick still dispatches the work.
        scheduler::switch_pending()
    };
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
            let state = sched.tasks[next as usize]
                .as_ref()
                .expect("current task has no TCB")
                .state;
            match state {
                TaskState::Ready => sched.set_current(next),
                TaskState::Running => {}
                _ => panic!("ARM scheduler selected a blocked current task"),
            }
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
