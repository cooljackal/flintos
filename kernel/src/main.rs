// SPDX-License-Identifier: Apache-2.0

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

use flint_hal::tick::TickSource;
use flint_hal::types::Priority;
use flint_arch_xtensa::tick::XtensaTick;
use flint_arch_xtensa::registers;

mod board;
mod counters;
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

// ── Bring-up boot diagnostics ────────────────────────────────────────────────
//
// This is a bring-up build: nothing here has ever run on real silicon, so a
// mis-behaving first boot needs to say what it was doing, not go silent. The
// banner below (`FlintMain`) prints known-good facts (measured clock, VECBASE,
// PS, SP, stack pool bounds, tick period) at each step so a hang or crash can
// be bisected against the last line printed, over raw UART0
// (`debug::fault::raw_print`/`raw_hex`/`raw_dec`), which works even if our own
// UART/console init is broken.
//
// To turn the banner OFF (e.g. once the board is known-good and boot chatter
// is just noise), flip this to `false`. There is no Cargo feature wired to it
// yet -- see the boot-diagnostics report for why.
const BOOT_DIAGNOSTICS: bool = true;

extern "C" {
    static _vector_table_start: u32;
    static _task_stack_start: u32;
    static _task_stack_end: u32;
}

// ── Panic handler ────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let msg = format_args!("{}", info.message());
    crate::debug::panic::handle(&msg);
}

// ── Demo task functions ──────────────────────────────────────────────────────
//
// Priorities live here as named constants, and `FlintMain` spawns each task
// with the matching constant, so the number a task logs about itself can
// never drift from the number the scheduler actually assigned it.
//
// Each line below carries: task name, its own priority (so interleaving vs.
// round-robin vs. one task hogging the CPU is legible straight from the log:
// same-priority tasks round-robin against each other, a higher-priority task
// preempts, and a monotonic per-task counter that stalls means that task
// stopped running, not that logging stopped), a per-task monotonic counter
// (proves *that* task's loop is still advancing, not just that *a* loop is),
// and the shared tick (proves they're all being scheduled off the same
// clock). A single task silently hogging the CPU shows up as the other two
// counters freezing while the tick keeps advancing.

const SENSOR_PRIORITY: Priority = Priority::Normal(1);
const CONSUMER_PRIORITY: Priority = Priority::Normal(5);
const HOUSEKEEP_PRIORITY: Priority = Priority::Background(1);

// Bring-up form: counters, not log lines.
//
// The logging path is several calls deep and writes to the same UART the trap
// handler's diagnostics use, so a task dying inside it cannot be told apart
// from a task that was never scheduled. A counter each proves scheduling on its
// own, with no shared resource and no call depth.
use core::sync::atomic::Ordering as O;

fn task_sensor() {
    loop {
        counters::SENSOR.fetch_add(1, O::Relaxed);
        flint_api::task::sleep_ms(500);
    }
}

fn task_consumer() {
    loop {
        counters::CONSUMER.fetch_add(1, O::Relaxed);
        flint_api::task::sleep_ms(1000);
    }
}

fn task_housekeep() {
    loop {
        flint_api::task::sleep_ms(3000);
        counters::HOUSEKEEP.fetch_add(1, O::Relaxed);
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
    // before we touch anything else. Raw UART0 (ROM-configured), so this
    // works even if our own UART/console init is broken or never runs.
    debug::fault::raw_print("\r\n[FLINT] FlintMain reached (_start -> Rust OK)\r\n");

    if BOOT_DIAGNOSTICS {
        // VECBASE and PS were both set by startup.S before it ever called
        // into Rust, so checking them here proves that asm did what it
        // claims -- not that this Rust code did.
        unsafe {
            let vecbase = registers::read_vecbase();
            let vector_table = core::ptr::addr_of!(_vector_table_start) as u32;
            debug::fault::raw_print("[FLINT] VECBASE=");
            debug::fault::raw_hex(vecbase);
            debug::fault::raw_print(" _vector_table_start=");
            debug::fault::raw_hex(vector_table);
            debug::fault::raw_print(if vecbase == vector_table {
                " MATCH (vector table installed)\r\n"
            } else {
                " MISMATCH -- vector table NOT installed, traps will go to ROM/garbage\r\n"
            });

            let ps = registers::read_ps();
            debug::fault::raw_print("[FLINT] PS=");
            debug::fault::raw_hex(ps);
            debug::fault::raw_print(if ps & registers::PS_WOE != 0 {
                " WOE=1 (window overflow/underflow enabled)\r\n"
            } else {
                " WOE=0 -- windowed calls will fault, nothing past this line is trustworthy\r\n"
            });

            let sp = registers::read_sp();
            let stack_start = core::ptr::addr_of!(_task_stack_start) as u32;
            let stack_end = core::ptr::addr_of!(_task_stack_end) as u32;
            debug::fault::raw_print("[FLINT] SP=");
            debug::fault::raw_hex(sp);
            debug::fault::raw_print(" task_stack_pool=[");
            debug::fault::raw_hex(stack_start);
            debug::fault::raw_print(", ");
            debug::fault::raw_hex(stack_end);
            debug::fault::raw_print(")\r\n");
        }
    }

    // Step 1: board init (UART console).
    startup::init();

    debug::fault::raw_print("[FLINT] startup::init done\r\n");

    // Step 2: tick timer (1 ms). Enables the Timer0 interrupt (still masked
    // until step 5). Also enable the software interrupt used by cooperative
    // switches (sleep/yield/block raise it via `scheduler::request_switch`).
    //
    // `XtensaTick::init` measures the real CPU frequency against the RTC
    // slow clock (issue #6) rather than assuming one; report what it found
    // before doing anything whose timing depends on it.
    // The board manifest owns the tick period. It declared TICK_PERIOD_US all
    // along and this call ignored it, hardcoding 1 ms -- so a board could not
    // actually change its own tick rate.
    //
    // Raising this is also the cleanest way to isolate a suspected register
    // window problem: at 115200 baud a log line takes several milliseconds to
    // shift out, so a 1 ms tick is guaranteed to interrupt one mid-write. If
    // output is truncated at 1 ms but complete at, say, 100 ms, the fault is in
    // what the trap does to the interrupted task's register windows (issue #1),
    // not in the logging path.
    XtensaTick::init(board::active::TICK_PERIOD_US);

    if BOOT_DIAGNOSTICS {
        let hz = XtensaTick::cpu_hz();
        debug::fault::raw_print("[FLINT] cpu_hz=");
        debug::fault::raw_dec(hz);
        debug::fault::raw_print(if XtensaTick::freq_measured() {
            " (measured: CCOUNT timed against RTC slow clock)\r\n"
        } else {
            " (ASSUMED -- RTC measurement failed or was implausible; \
              falling back to the hardcoded constant, which may be WRONG)\r\n"
        });
        debug::fault::raw_print("[FLINT] tick period=");
        debug::fault::raw_dec(XtensaTick::ticks_per_period());
        debug::fault::raw_print(" CCOUNT ticks\r\n");
    }

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
    // 4 KiB rather than 2 KiB. A trap lands on the interrupted task's own
    // stack, and with logging enabled the interrupted call chain can already be
    // several frames deep (log_info! -> formatting -> the UART driver) before
    // the trap frame and _flint_trap's own frame are added on top. None of that
    // has ever run on hardware. The stack pool is 96 KiB and three tasks use
    // 12 KiB of it, so the headroom is free -- and a stack overflow here would
    // look very much like the missing window spill (issue #1), which is exactly
    // the confusion to avoid on a first bring-up.
    flint_api::task::spawn("sensor", task_sensor, SENSOR_PRIORITY, 4096);
    flint_api::task::spawn("consumer", task_consumer, CONSUMER_PRIORITY, 4096);
    flint_api::task::spawn("housekeep", task_housekeep, HOUSEKEEP_PRIORITY, 4096);

    flint_api::log_info!("[kernel] Flint RTOS boot complete, entering idle");

    // Step 5: unmask interrupts. The next tick will preempt idle and start the
    // highest-priority ready task via the trap handler. This is the
    // highest-risk moment in the whole boot: everything up to here is plain
    // sequential code, but the very first interrupt now has to survive the
    // trap entry/exit asm and dispatch through the scheduler for the first
    // time ever, on real hardware. If the board dies silently, it died
    // between these two lines.
    if BOOT_DIAGNOSTICS {
        debug::fault::raw_print("[FLINT] unmasking interrupts...\r\n");
    }
    let _prev = unsafe { registers::set_intlevel_0() };
    if BOOT_DIAGNOSTICS {
        debug::fault::raw_print("[FLINT] interrupts unmasked, entering idle\r\n");
    }

    // Step 6: become the idle task.
    idle_loop();
}

/// Trampoline so the idle TCB has a non-null `entry`. Never actually dispatched
/// to (idle's context is the live boot context), but keeps the TCB well-formed.
fn idle_loop_entry() {
    idle_loop();
}
