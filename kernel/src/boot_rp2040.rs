// SPDX-License-Identifier: Apache-2.0

//! RP2040 core-0 kernel initialization.
//!
//! Reset enters with one live core. A single outer architecture critical
//! section keeps SysTick and PendSV from observing half-built scheduler state;
//! nested scheduler locks preserve that original PRIMASK token. Releasing the
//! outer section is the only interrupt-enable operation in this path.

#[cfg(target_os = "none")]
use hal::arch::Architecture;
#[cfg(target_os = "none")]
use hal::tick::TickSource;

#[cfg(target_os = "none")]
use crate::arch::Tick;
#[cfg(any(target_os = "none", test))]
use crate::board;
#[cfg(target_os = "none")]
use crate::scheduler;

#[cfg(target_os = "none")]
unsafe extern "C" {
    fn flint_app_main();
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootStep {
    Clock,
    Heap,
    Tick,
    Idle,
    Application,
    Interrupts,
}

#[cfg(test)]
const BOOT_ORDER: [BootStep; 6] = [
    BootStep::Clock,
    BootStep::Heap,
    BootStep::Tick,
    BootStep::Idle,
    BootStep::Application,
    BootStep::Interrupts,
];

/// Prepare kernel state before the reset handler enters the first task by SVC.
#[cfg(target_os = "none")]
#[no_mangle]
pub extern "C" fn _flint_armv6m_boot() {
    // A watchdog or software reset can leave an RP2040 hardware spinlock
    // claimed. Pico SDK resets all 32 before its runtime enters main; Flint
    // releases its one owned lock before attempting the outer boot section.
    unsafe { crate::arch::init_boot_core() };

    // This token records the reset handler's PRIMASK state. Every scheduler
    // lock taken below nests inside it and restores to "still masked".
    let boot_primask = unsafe { crate::arch::cs_enter() };

    crate::startup::init();
    unsafe {
        use hal::soc::SystemOnChip;
        board::SelectedSoc::configure_cpu_clock();
    }

    // The linker-defined task-stack pool is consumed lazily by spawn. The
    // remaining SRAM above the static DMA pool backs runtime kernel objects.
    unsafe { crate::heap::init_from_map() };

    Tick::init(
        board::active::TICK_PERIOD_US,
        <board::SelectedSoc as hal::soc::SystemOnChip>::DEFAULT_CPU_HZ,
    );
    install_idle_task();
    unsafe { flint_app_main() };

    // Release the outer RP2040 spinlock and restore Reset's PRIMASK. Reset
    // immediately enters SVC, which selects and exception-returns into the
    // highest-priority ready task.
    unsafe { crate::arch::cs_exit(boot_primask) };
}

#[cfg(target_os = "none")]
fn install_idle_task() {
    scheduler::with(|sched| {
        let id = sched.alloc_id().expect("no idle slot");
        debug_assert_eq!(id, 0);
        const IDLE_STACK_SIZE: u32 = 1024;
        let stack_base = crate::spawn::allocate_stack(IDLE_STACK_SIZE).expect("no ARM idle stack");
        crate::spawn::paint_stack(stack_base, IDLE_STACK_SIZE);
        let tcb = sched.tasks[0].as_mut().expect("idle TCB missing");
        tcb.name = "idle";
        tcb.entry = Some(idle_loop_entry);
        tcb.base_prio = scheduler::IDLE_PRIORITY;
        tcb.priority = scheduler::IDLE_PRIORITY;
        tcb.state = scheduler::TaskState::Ready;
        tcb.quantum = scheduler::DEFAULT_QUANTUM_MS;
        tcb.stack_base = stack_base;
        tcb.stack_size = IDLE_STACK_SIZE;
        tcb.affinity = scheduler::Affinity::Core(hal::smp::CoreId::BOOT);
        unsafe {
            crate::arch::SelectedArch::init_context(
                &mut tcb.context,
                idle_loop_entry as *const () as usize,
                stack_base + IDLE_STACK_SIZE,
            )
        };
        sched.ready_mask |= 1u64 << scheduler::IDLE_PRIORITY;
    });
}

#[cfg(target_os = "none")]
fn idle_loop() -> ! {
    loop {
        crate::dynobj::reap_deleted();
        crate::arch::wait_for_interrupt();
    }
}

#[cfg(target_os = "none")]
fn idle_loop_entry() {
    idle_loop();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupts_are_the_last_boot_transition() {
        assert_eq!(BOOT_ORDER.last(), Some(&BootStep::Interrupts));
        assert!(
            BOOT_ORDER.iter().position(|s| *s == BootStep::Idle)
                < BOOT_ORDER.iter().position(|s| *s == BootStep::Application)
        );
    }

    #[test]
    fn configured_tick_has_a_valid_systick_reload() {
        use hal::soc::SystemOnChip;
        let ticks = u64::from(board::SelectedSoc::DEFAULT_CPU_HZ)
            * u64::from(board::active::TICK_PERIOD_US)
            / 1_000_000;
        assert!((1..=0x0100_0000).contains(&ticks));
    }
}
