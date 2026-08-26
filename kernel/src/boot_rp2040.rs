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
use portable_atomic::{AtomicU32, Ordering};

#[cfg(target_os = "none")]
unsafe extern "C" {
    fn flint_app_main();
    static _vector_table_start: u8;
    static _core1_boot_stack_top: u8;
    fn _flint_armv6m_core1_entry();
}

#[cfg(target_os = "none")]
pub(crate) static CORE1_IDLE: AtomicU32 = AtomicU32::new(u32::MAX);

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

    #[cfg(not(feature = "rp2040-test-recovery"))]
    unsafe {
        soc_rp2040::watchdog::disarm();
        soc_rp2040::watchdog::clear_flint_watchdog_marker();
    }

    // This token records the reset handler's PRIMASK state. Every scheduler
    // lock taken below nests inside it and restores to "still masked".
    let boot_primask = unsafe { crate::arch::cs_enter() };

    unsafe {
        use hal::soc::SystemOnChip;
        board::SelectedSoc::configure_cpu_clock();
    }
    crate::startup::init();
    crate::debug::panic::report_previous();
    // The linker-defined task-stack pool is consumed lazily by spawn. The
    // remaining SRAM above the static DMA pool backs runtime kernel objects.
    unsafe { crate::heap::init_from_map() };

    let (cpu_hz, provenance) = select_cpu_clock(
        <board::SelectedSoc as hal::soc::SystemOnChip>::measure_cpu_hz(
            crate::arch::SelectedArch::cycle_count,
        ),
    );
    // The console is already initialized and board-owned. The raw fault
    // reporter is ESP32-specific and must not be used from this ARM path.
    report_cpu_clock(&mut crate::debug::console::Console, cpu_hz, provenance);
    Tick::init(board::active::TICK_PERIOD_US, cpu_hz);
    install_idle_task();
    let core1_idle = install_core1_idle_task();
    CORE1_IDLE.store(core1_idle, Ordering::Release);

    // Core 1 must take the scheduler spinlock to publish its join. Release the
    // boot-wide lock before launch or the two cores deadlock here.
    unsafe { crate::arch::cs_exit(boot_primask) };
    launch_core1();
    unsafe { flint_app_main() };
}

#[cfg(any(target_os = "none", test))]
fn select_cpu_clock(measured: Option<u32>) -> (u32, &'static str) {
    match measured.filter(|hz| *hz != 0) {
        Some(hz) => (hz, " (measured against crystal-backed reference)\r\n"),
        None => (
            <board::SelectedSoc as hal::soc::SystemOnChip>::DEFAULT_CPU_HZ,
            " (ASSUMED: measurement unavailable; configured fallback, not measured)\r\n",
        ),
    }
}

#[cfg(any(target_os = "none", test))]
fn report_cpu_clock(console: &mut impl core::fmt::Write, hz: u32, provenance: &str) {
    let _ = write!(console, "[FLINT] cpu_hz={hz}{provenance}");
}

#[cfg(target_os = "none")]
fn launch_core1() {
    unsafe {
        soc_rp2040::multicore::launch_core1(
            core::ptr::addr_of!(_vector_table_start) as u32,
            core::ptr::addr_of!(_core1_boot_stack_top) as u32,
            _flint_armv6m_core1_entry as *const () as u32,
        )
        .expect("RP2040 core 1 launch failed");
    }
    super::armv6m::enable_sio_fifo_irq(soc_rp2040::multicore::IRQ_SIO_PROC0);

    for _ in 0..1_000_000 {
        if crate::smp::is_pinnable(1) {
            return;
        }
        core::hint::spin_loop();
    }
    panic!("RP2040 core 1 did not join the scheduler");
}

#[cfg(target_os = "none")]
fn install_core1_idle_task() -> u32 {
    scheduler::with(|sched| {
        let id = sched.alloc_id().expect("no core-1 idle slot");
        const IDLE_STACK_SIZE: u32 = 1024;
        let stack_base =
            crate::spawn::allocate_stack(IDLE_STACK_SIZE).expect("no ARM core-1 idle stack");
        crate::spawn::paint_stack(stack_base, IDLE_STACK_SIZE);
        let tcb = sched.tasks[id as usize]
            .as_mut()
            .expect("core-1 idle TCB missing");
        tcb.name = "idle1";
        tcb.entry = Some(idle_loop_entry);
        tcb.base_prio = scheduler::IDLE_PRIORITY;
        tcb.priority = scheduler::IDLE_PRIORITY;
        tcb.state = scheduler::TaskState::Ready;
        tcb.quantum = scheduler::DEFAULT_QUANTUM_MS;
        tcb.stack_base = stack_base;
        tcb.stack_size = IDLE_STACK_SIZE;
        tcb.affinity = scheduler::Affinity::Core(hal::smp::CoreId(1));
        unsafe {
            crate::arch::SelectedArch::init_context(
                &mut tcb.context,
                idle_loop_entry as *const () as usize,
                stack_base + IDLE_STACK_SIZE,
            )
        };
        sched.ready_mask |= 1u64 << scheduler::IDLE_PRIORITY;
        id
    })
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

    #[test]
    fn measured_clock_is_used_and_labelled_as_measured() {
        for hz in [12_001_000, 124_999_000] {
            let (selected, label) = select_cpu_clock(Some(hz));
            assert_eq!(selected, hz);
            assert!(label.starts_with(" (measured against"));
        }
    }

    #[test]
    fn missing_or_zero_measurement_is_explicitly_assumed() {
        use hal::soc::SystemOnChip;
        for sample in [None, Some(0)] {
            let (selected, label) = select_cpu_clock(sample);
            assert_eq!(selected, board::SelectedSoc::DEFAULT_CPU_HZ);
            assert!(label.contains("ASSUMED"));
            assert!(label.contains("not measured"));
        }
    }

    #[test]
    fn clock_report_uses_the_console_writer_with_measured_or_assumed_provenance() {
        for measurement in [Some(12_001_000), None] {
            let (hz, provenance) = select_cpu_clock(measurement);
            let mut console = std::string::String::new();
            report_cpu_clock(&mut console, hz, provenance);
            assert_eq!(console, std::format!("[FLINT] cpu_hz={hz}{provenance}"));
            assert_eq!(console.contains("ASSUMED:"), measurement.is_none());
        }
    }
}
