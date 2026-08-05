// SPDX-License-Identifier: Apache-2.0

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

use flint_hal::tick::TickSource;
use flint_hal::types::Priority;
use flint_arch_xtensa::tick::XtensaTick;
use flint_arch_xtensa::registers;

mod board;
mod debug;
mod dma_broker;
mod interrupt;
mod mutex;
mod queue;
mod scheduler;
mod spawn;
mod startup;
mod switch;
mod syscall;
mod timer;

// ── Panic handler ────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let msg = format_args!("{}", info.message());
    crate::debug::panic::handle(&msg);
}

// ── Demo task functions ──────────────────────────────────────────────────────

fn task_sensor() {
    let mut count = 0u32;
    loop {
        count += 1;
        flint_api::log_info!("[sensor] reading #{} tick={}", count, flint_api::timer::now_ms());
        flint_api::task::sleep_ms(500);
    }
}

fn task_consumer() {
    loop {
        flint_api::log_info!("[consumer] processing tick={}", flint_api::timer::now_ms());
        flint_api::task::sleep_ms(1000);
    }
}

fn task_housekeep() {
    loop {
        flint_api::task::sleep_ms(3000);
        flint_api::log_info!("[housekeep] alive tick={}", flint_api::timer::now_ms());
    }
}

/// The idle task body. Runs at the lowest priority on the boot/kernel stack;
/// `waiti 0` parks the CPU until the next interrupt, which drives scheduling.
fn idle_loop() -> ! {
    loop {
        unsafe { core::arch::asm!("waiti 0") };
    }
}

// ── Entry point ──────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn FlintMain() -> ! {
    // Bring-up marker: confirms _start + the asm→Rust windowed call worked,
    // before we touch anything else. Raw UART0 (ROM-configured).
    debug::fault::raw_print("\r\n[FLINT] FlintMain reached\r\n");

    // Step 1: board init (UART console).
    startup::init();

    debug::fault::raw_print("[FLINT] startup::init done\r\n");

    // Step 2: tick timer (1 ms). Enables the Timer0 interrupt (still masked
    // until step 5). Also enable the software interrupt used by cooperative
    // switches (sleep/yield/block raise it via `scheduler::request_switch`).
    XtensaTick::init(1000);
    unsafe { registers::enable_interrupt(registers::INT_SOFTWARE) };

    // Step 3: install the idle task as TCB 0 and make it `current`. It runs on
    // the current (kernel/boot) stack — `FlintMain` itself becomes idle once
    // interrupts are enabled. (Plan W3.3: a real idle TCB, not a relabelled
    // user task.)
    scheduler::with(|sched| {
        let id = sched.alloc_id().expect("no idle slot");
        debug_assert_eq!(id, 0);
        if let Some(tcb) = &mut sched.tasks[0] {
            tcb.name = "idle";
            tcb.entry = Some(idle_loop_entry);
            tcb.base_prio = scheduler::IDLE_PRIORITY;
            tcb.priority = scheduler::IDLE_PRIORITY;
            tcb.state = scheduler::TaskState::Running;
            tcb.quantum = scheduler::DEFAULT_QUANTUM_MS;
            tcb.stack_size = 0; // runs on the kernel stack; HWM scan skipped
        }
        sched.ready_mask |= 1u64 << scheduler::IDLE_PRIORITY;
        sched.set_current(0);
    });

    // Step 4: spawn the demo tasks.
    flint_api::task::spawn("sensor", task_sensor, Priority::Normal(1), 2048);
    flint_api::task::spawn("consumer", task_consumer, Priority::Normal(5), 2048);
    flint_api::task::spawn("housekeep", task_housekeep, Priority::Background(1), 2048);

    flint_api::log_info!("[kernel] Flint RTOS boot complete, entering idle");

    // Step 5: unmask interrupts. The next tick will preempt idle and start the
    // highest-priority ready task via the trap handler.
    let _prev = unsafe { registers::set_intlevel_0() };

    // Step 6: become the idle task.
    idle_loop();
}

/// Trampoline so the idle TCB has a non-null `entry`. Never actually dispatched
/// to (idle's context is the live boot context), but keeps the TCB well-formed.
fn idle_loop_entry() {
    idle_loop();
}
