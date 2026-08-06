// SPDX-License-Identifier: Apache-2.0

//! Boot sequence and the kernel's entry point.
//!
//! `_start` (in `startup.S`) calls [`FlintMain`], which brings the system up
//! and then hands control to the application: exactly one crate in the build
//! defines `flint_app_main`, normally via the [`flint_app!`] macro.
//!
//! Everything above that line is the kernel's business — console, tick timer,
//! idle task, interrupt unmasking. Everything below it is the application's.

use crate::arch::registers;
use crate::arch::Tick;
use hal::tick::TickSource;

use crate::{board, debug, scheduler};

/// Print the boot banner over raw UART0.
///
/// This is a young kernel on a target where a silent failure is the default
/// outcome, so each line reports a fact the *previous* step was responsible
/// for: `VECBASE` and `PS` were set by `startup.S` before Rust ran, the stack
/// pointer says which stack we are on, the clock is measured rather than
/// assumed. A hang can then be bisected against the last line printed.
///
/// It goes out over raw UART0 (ROM-configured), so it works even if our own
/// UART or console init is broken or never runs.
///
/// Set to `false` once a board is known-good and the boot chatter is noise.
/// See also `switch::TRAP_DIAGNOSTICS` for the trap-path equivalent.
pub const BOOT_DIAGNOSTICS: bool = true;

extern "C" {
    static _vector_table_start: u32;
    static _task_stack_start: u32;
    static _task_stack_end: u32;

    /// The application entry point. Defined by exactly one crate in the build;
    /// see [`flint_app!`].
    fn flint_app_main();
}

/// Declare a function as this build's application entry point.
///
/// A Flint application is a `no_std` binary crate that links the kernel and
/// names one function to run once the system is up. That function spawns the
/// application's tasks and returns; the kernel then unmasks interrupts and
/// becomes the idle task.
///
/// ```ignore
/// #![no_std]
/// #![no_main]
///
/// kernel::flint_app!(main);
///
/// fn main() {
///     api::task::spawn("blink", blink, Priority::Normal(1), 4096);
/// }
/// ```
#[macro_export]
macro_rules! flint_app {
    ($main:path) => {
        #[no_mangle]
        pub extern "C" fn flint_app_main() {
            // Bind through a `fn()` so the macro rejects a signature the kernel
            // cannot call, at the definition site rather than at link time.
            let entry: fn() = $main;
            entry();
        }
    };
}

/// Kernel entry point. Called by `_start` in `startup.S`.
#[no_mangle]
pub extern "C" fn FlintMain() -> ! {
    if BOOT_DIAGNOSTICS {
        // Confirms _start and the asm→Rust windowed call worked, before we
        // touch anything else.
        debug::fault::raw_print("\r\n[FLINT] FlintMain reached (_start -> Rust OK)\r\n");
        report_boot_state();
    }

    // Step 1: board init (UART console).
    crate::startup::init();

    if BOOT_DIAGNOSTICS {
        debug::fault::raw_print("[FLINT] startup::init done\r\n");
    }

    // If the last boot panicked, say so now that there is a console to say it
    // on. The snapshot lives in a region the linker keeps out of .bss, so it
    // survives a soft reset -- which is the only reason writing it was ever
    // worth doing. Nothing read it until this call existed.
    debug::panic::report_previous();

    // Step 2: tick timer. Enables the Timer0 interrupt (still masked until step
    // 5). Also enable the software interrupt used by cooperative switches
    // (sleep/yield/block raise it via `scheduler::request_switch`).
    //
    // The period comes from the board manifest, which has always declared
    // `TICK_PERIOD_US`. `Tick::init` measures the real CPU frequency
    // against the RTC slow clock rather than assuming one, so report what it
    // found before anything whose timing depends on it runs.
    Tick::init(board::active::TICK_PERIOD_US);

    if BOOT_DIAGNOSTICS {
        report_clock();
    }

    unsafe { registers::enable_interrupt(registers::INT_SOFTWARE) };

    // Step 3: install the idle task as TCB 0 and make it `current`. It runs on
    // the current (kernel/boot) stack — `FlintMain` itself becomes idle once
    // interrupts are enabled.
    install_idle_task();

    // Step 4: hand over to the application, which spawns its own tasks.
    unsafe { flint_app_main() };

    #[cfg(feature = "flint-log")]
    api::log_info!("[kernel] Flint RTOS boot complete, entering idle");

    // A build with logging compiled out looks exactly like a board that boots
    // and then dies: the banner appears, the tasks run, and nothing they print
    // reaches the console because there is no print. Say so once, over raw
    // UART0, which does not depend on the feature that is missing.
    #[cfg(not(feature = "flint-log"))]
    debug::fault::raw_print(
        "[FLINT] boot complete. Logging is COMPILED OUT (debug-level-0) -- tasks will \
         run but print nothing.\r\n[FLINT] Rebuild with DEBUG=debug-level-1 to see task \
         output.\r\n",
    );

    // Step 5: unmask interrupts. The next tick will preempt idle and start the
    // highest-priority ready task via the trap handler. Everything up to here
    // is plain sequential code; from here on the trap entry/exit asm and the
    // scheduler are load-bearing. If a board dies silently, it died between
    // these two lines.
    if BOOT_DIAGNOSTICS {
        debug::fault::raw_print("[FLINT] unmasking interrupts...\r\n");
    }
    let _prev = unsafe { registers::set_intlevel_0() };
    if BOOT_DIAGNOSTICS {
        debug::fault::raw_print("[FLINT] interrupts unmasked, entering idle\r\n");
    }

    // Boot-time self-test, off unless the `self-test` feature is on.
    //
    // It runs here, after unmasking and before idle, because that is the only
    // point where it means anything: it drives a deep windowed recursion and
    // checks the result, so the timer ISR has to be able to interrupt it
    // mid-computation. That is precisely the failure that cost this kernel its
    // longest bring-up bug -- a trap corrupting the interrupted task's register
    // windows -- and this is the regression test for it.
    #[cfg(feature = "self-test")]
    crate::selftest::run();

    // Step 6: become the idle task.
    idle_loop();
}

/// Report what `startup.S` left behind: vector table, window state, stack.
fn report_boot_state() {
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

/// Report the measured CPU frequency and the resulting tick period.
///
/// Whether the frequency was measured or fallen back on matters more than the
/// number itself: every timeout in the system is scaled by it, so an assumed
/// value produces a kernel whose delays are all wrong by the same factor.
fn report_clock() {
    debug::fault::raw_print("[FLINT] cpu_hz=");
    debug::fault::raw_dec(Tick::cpu_hz());
    debug::fault::raw_print(if Tick::freq_measured() {
        " (measured: CCOUNT timed against RTC slow clock)\r\n"
    } else {
        " (ASSUMED -- RTC measurement failed or was implausible; \
          falling back to the hardcoded constant, which may be WRONG)\r\n"
    });
    debug::fault::raw_print("[FLINT] tick period=");
    debug::fault::raw_dec(Tick::ticks_per_period());
    debug::fault::raw_print(" CCOUNT ticks\r\n");
}

/// Install idle as TCB 0 and make it the running task.
///
/// Idle is a real TCB rather than a relabelled application task, and it runs on
/// the boot stack: `FlintMain` *is* idle once interrupts are unmasked, so there
/// is no stack to allocate and nothing to paint. `stack_size = 0` tells the
/// high-water scan to skip it.
fn install_idle_task() {
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
            tcb.stack_size = 0;
        }
        sched.ready_mask |= 1u64 << scheduler::IDLE_PRIORITY;
        sched.set_current(0);
    });
}

/// The idle task body. Runs at the lowest priority on the boot/kernel stack;
/// `waiti 0` parks the CPU until the next interrupt, which drives scheduling.
fn idle_loop() -> ! {
    loop {
        crate::arch::wait_for_interrupt();
    }
}

/// Trampoline so the idle TCB has a non-null `entry`. Never actually dispatched
/// to (idle's context is the live boot context), but keeps the TCB well-formed.
fn idle_loop_entry() {
    idle_loop();
}
