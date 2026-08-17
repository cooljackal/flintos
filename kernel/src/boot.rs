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
#[cfg(target_os = "none")]
use crate::clock;

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
/// A FlintOS application is a `no_std` binary crate that links the kernel and
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
    ($main:path, abi = $abi:literal) => {
        // Checked at compile time, so an application written against an older
        // FlintOS fails to build with something that names the problem, instead
        // of erroring deep inside a call it did not change.
        const _: () = {
            if $abi != $crate::ABI {
                ::core::panic!(::core::concat!(
                    "FlintOS ABI mismatch: this application declares `abi = ",
                    ::core::stringify!($abi),
                    "`, which is not the ABI this kernel provides (see `api::ABI`).\n",
                    "The application-facing API changed incompatibly since it was \
                     written. Read the Breaking entries in CHANGELOG.md, apply them, \
                     then update the declaration in `flint_app!`.\n",
                    "`make upgrade` reports which applications this affects."
                ));
            }
        };

        #[no_mangle]
        pub extern "C" fn flint_app_main() {
            // Bind through a `fn()` so the macro rejects a signature the kernel
            // cannot call, at the definition site rather than at link time.
            let entry: fn() = $main;
            entry();
        }
    };

    // Without the declaration there is nothing to check, and an unversioned
    // application is exactly the one that breaks silently. Say so here rather
    // than letting `macro_rules!` report "no rules expected this token".
    ($main:path) => {
        ::core::compile_error!(
            "flint_app! needs the ABI this application was written against:\n\
             \n    kernel::flint_app!(main, abi = 1);\n\n\
             It is checked at build time so a kernel upgrade that changes the \
             application-facing API fails here, naming the cause, instead of \
             somewhere in your own code."
        );
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
        report_reset_cause();
    }

    // Step 1: board init (UART console).
    crate::startup::init();

    if BOOT_DIAGNOSTICS {
        debug::fault::raw_print("[FLINT] startup::init done\r\n");
    }

    // Step 1b: raise the CPU to 240 MHz. The bootloader hands off at 80 MHz and
    // expects the application to do this; the Wi-Fi blob is built and timed for
    // 240. APB stays 80 MHz, so the console just opened is unaffected. Done here
    // — single-core, before the frequency is measured below, before the radio
    // (the other PLL user) exists — so nothing races the clock switch.
    #[cfg(target_os = "none")]
    unsafe {
        soc_esp32::cpu_clk::set_240mhz();
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
    // `TICK_PERIOD_US`. The frequency is measured rather than assumed, and
    // measured *here* rather than inside the tick source -- see
    // `measure_cpu_hz`. Report what it found before anything whose timing
    // depends on it runs.
    // The microsecond clock, before the tick: `clock::init` claims TIMG1/T1
    // and nothing else may drive it, and doing it here means it is running
    // before any second core exists to race the one write it makes.
    #[cfg(target_os = "none")]
    if !unsafe { clock::init() } {
        debug::fault::raw_print("[FLINT] TIMG1/T1 unavailable: now_us() will read 0

");
    }

    let (cpu_hz, measured) = measure_cpu_hz();
    CPU_HZ_MEASURED.store(measured, core::sync::atomic::Ordering::Relaxed);
    Tick::init(board::active::TICK_PERIOD_US, cpu_hz);

    if BOOT_DIAGNOSTICS {
        report_clock();
    }

    unsafe { registers::enable_interrupt(registers::INT_SOFTWARE) };

    // Step 2b: tell the flash driver which interrupts it may leave enabled.
    //
    // Before this, a flash erase masked everything for its whole duration --
    // tens of milliseconds with no tick and no driver interrupt, which is a
    // real-time defect on its own terms and fatal to a radio link. The driver
    // is Layer 1 and may not name `kernel`, so the register of IRAM-safe
    // handlers is handed over rather than reached for.
    //
    // Installed before any task runs, so no flash operation can happen with
    // the hook half-set.
    #[cfg(target_os = "none")]
    unsafe {
        esp32_flash::set_interrupt_hooks(
            crate::interrupt::mask_non_iram_safe,
            crate::interrupt::restore_mask,
        );
    }

    // Step 3: install the idle task as TCB 0 and make it `current`. It runs on
    // the current (kernel/boot) stack — `FlintMain` itself becomes idle once
    // interrupts are enabled.
    install_idle_task();

    // Step 4: hand over to the application, which spawns its own tasks.
    unsafe { flint_app_main() };

    #[cfg(feature = "flint-log")]
    api::log_info!("[kernel] FlintOS boot complete, entering idle");

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

/// Say why the chip last reset.
///
/// Unconditional, not gated on a diagnostic flag: after an unexplained reboot
/// this is the first thing worth knowing, and it is one line. Three watchdogs
/// exist and are armed for different reasons, so "a watchdog did it" is not an
/// answer -- the register distinguishes them and this prints which.
fn report_reset_cause() {
    let cause = unsafe { soc_esp32::reset::cause() };
    debug::fault::raw_print("[FLINT] reset cause=");
    debug::fault::raw_dec(cause);
    debug::fault::raw_print(" (");
    debug::fault::raw_print(soc_esp32::reset::name(cause));
    debug::fault::raw_print(")
");
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
/// Whether [`measure_cpu_hz`] got a real answer or fell back.
static CPU_HZ_MEASURED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Time CCOUNT against the RTC slow clock to find the CPU frequency.
///
/// Returns the frequency and whether it was actually measured; on failure the
/// SoC's documented fallback and `false`, because a wrong clock scales every
/// timeout in the system and silently using one is what caused issue #6.
///
/// # Why this lives in the kernel
///
/// It needs a cycle counter and a reference clock: `arch::registers` has the
/// first, `soc_esp32::rtc` the second, and neither crate may name the other --
/// `arch/*` and `soc/*` both depend on `hal` and nothing else. `arch-xtensa`
/// used to do it anyway by carrying its own copy of RTC_CNTL's base address
/// and offsets, which put an ESP32 peripheral inside the crate whose subject
/// is a CPU core several chips share.
///
/// The kernel is the one place allowed to name both, so the measurement
/// belongs here and `TickSource::init` takes the answer.
#[cfg(target_os = "none")]
fn measure_cpu_hz() -> (u32, bool) {
    use soc_esp32::rtc;

    /// RTC slow-clock ticks to measure across. ~1500 is ~10 ms at the nominal
    /// 150 kHz: long enough to average out RC jitter, short enough not to
    /// visibly delay boot.
    const MEASURE_RTC_TICKS: u64 = 1500;
    /// Bound on CCOUNT cycles spent waiting for the RTC counter to move, so a
    /// stuck RTC block cannot hang boot. 50M cycles is ~0.6 s at 80 MHz, and
    /// nowhere near the ~4.29e9 where a 32-bit CCOUNT read could wrap.
    const MEASURE_TIMEOUT_CYCLES: u32 = 50_000_000;
    /// Polls allowed per RTC sample before calling the clock stopped.
    const RTC_POLLS: u32 = 10_000;

    let measured = (|| unsafe {
        let rtc0 = rtc::counter(RTC_POLLS)?;
        let c0 = registers::read_ccount();
        loop {
            let elapsed_rtc = rtc::counter(RTC_POLLS)?.wrapping_sub(rtc0);
            if elapsed_rtc >= MEASURE_RTC_TICKS {
                let cycles = registers::read_ccount().wrapping_sub(c0) as u64;
                return rtc::round_to_plausible(cycles * rtc::SLOW_HZ_NOMINAL / elapsed_rtc);
            }
            if registers::read_ccount().wrapping_sub(c0) > MEASURE_TIMEOUT_CYCLES {
                return None; // the RTC counter never moved
            }
        }
    })();

    match measured {
        Some(hz) => (hz, true),
        None => (rtc::DEFAULT_CPU_HZ, false),
    }
}

/// Host stand-in: there is no CCOUNT and no RTC to time it against.
#[cfg(not(target_os = "none"))]
fn measure_cpu_hz() -> (u32, bool) {
    (80_000_000, false)
}

fn report_clock() {
    debug::fault::raw_print("[FLINT] cpu_hz=");
    debug::fault::raw_dec(Tick::cpu_hz());
    debug::fault::raw_print(if CPU_HZ_MEASURED.load(core::sync::atomic::Ordering::Relaxed) {
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
            // Pinned, and it matters. Idle runs on its core's own stack and
            // has no saved context to resume from, so the other core picking
            // it up would resume a stack it does not own.
            tcb.affinity = scheduler::Affinity::Core(hal::smp::CoreId::BOOT);
        }
        sched.ready_mask |= 1u64 << scheduler::IDLE_PRIORITY;
        sched.set_current(0);
    });
}

/// The idle task body. Runs at the lowest priority on the boot/kernel stack;
/// `waiti 0` parks the CPU until the next interrupt, which drives scheduling.
fn idle_loop() -> ! {
    loop {
        // Proof that scheduling still reaches the bottom of the priority
        // range. A task that never yields starves this loop, and the watchdog
        // it feeds is the only thing that notices -- the tick keeps running
        // throughout, so nothing else in the system sees a problem.
        crate::watchdog::feed_from_idle();

        // Reap tasks that deleted themselves. Idle is the one context that is
        // provably not running on a dying task's stack — it runs on the boot
        // stack, and is only reached when nothing else is ready — which is
        // what makes freeing safe here and nowhere else.
        //
        // Cheap when there is nothing to do: one pass over the TCBs looking
        // for a state that is almost never set.
        crate::dynobj::reap_deleted();

        crate::arch::wait_for_interrupt();
    }
}

/// Trampoline so the idle TCB has a non-null `entry`. Never actually dispatched
/// to (idle's context is the live boot context), but keeps the TCB well-formed.
fn idle_loop_entry() {
    idle_loop();
}

// ── Secondary core ──────────────────────────────────────────────────────────

/// The registered form of `esp32_flash::park_this_core`.
///
/// A top-half is a safe `fn()`, and the driver's entry point is `unsafe`
/// because of where it may be called from. In IRAM for the obvious reason: it
/// is the first thing on the path that runs with the cache off, and a shim in
/// flash would defeat the function it is shimming.
#[cfg(target_os = "none")]
#[inline(never)]
#[link_section = ".iram1.flash"]
fn park_isr() {
    unsafe { esp32_flash::park_this_core() };
}

/// Bring a secondary core into the scheduler.
///
/// Called *on that core*, by the trampoline, once it has a stack. When this
/// returns the core is a full peer: it takes traps, ticks, and schedules.
///
/// The order is the whole of it. Each step makes the next one survivable:
///
/// 1. **Vector table.** Until `VECBASE` points somewhere real, any exception —
///    including the first tick — goes wherever reset left it.
/// 2. **An idle task, pinned here.** A core with nothing to run still needs a
///    `current`: the trap handler saves the outgoing context into it. Pinned,
///    because idle runs on this core's own stack.
/// 3. **This core's timer.** `CCOUNT`, `CCOMPARE0` and `INTENABLE` are all
///    per-core. The shared tick count is *not* advanced here — see
///    `XtensaTick::tick`.
/// 4. **Unmask.** Only now, with a handler installed and a task to switch away
///    from.
///
/// # Safety
/// Runs on a core that is not yet scheduling, with interrupts masked.
#[cfg(target_os = "none")]
pub unsafe fn join_scheduler() -> ! {
    extern "C" {
        static _vector_table_start: u32;
    }
    registers::set_vecbase(core::ptr::addr_of!(_vector_table_start) as u32);

    let me = crate::smp::current_core();
    scheduler::with(|sched| {
        let id = sched.alloc_id().expect("no TCB slot for a secondary idle task");
        if let Some(tcb) = &mut sched.tasks[id as usize] {
            tcb.name = "idle1";
            tcb.entry = Some(idle_loop_entry);
            tcb.base_prio = scheduler::IDLE_PRIORITY;
            tcb.priority = scheduler::IDLE_PRIORITY;
            tcb.state = scheduler::TaskState::Running;
            tcb.quantum = scheduler::DEFAULT_QUANTUM_MS;
            tcb.stack_size = 0;
            tcb.affinity = scheduler::Affinity::Core(me);
        }
        sched.ready_mask |= 1u64 << scheduler::IDLE_PRIORITY;
        sched.set_current(id);
    });

    Tick::init_this_core();

    // The software interrupt too, and it is not optional. `request_switch`
    // raises it to make a cooperative switch happen -- every `sleep_ms`,
    // `yield_now` and blocking send goes through it. Masked, the call returns
    // and the task keeps running with its TCB already marked blocked, until
    // the next timer tick happens to preempt it.
    //
    // That is exactly what the second core did before this line existed: a
    // task sleeping 7 ms iterated ten thousand times a second. `INTENABLE` is
    // per-core, so core 0 enabling it says nothing about core 1.
    registers::enable_interrupt(registers::INT_SOFTWARE);

    // How core 0 asks this core to get out of the way for a flash operation.
    //
    // Before this, it did not ask: `esp32-flash` stalled the APP CPU in
    // hardware, freezing it wherever it happened to be — including, in
    // principle, holding a `Spinlock` that core 0 was about to want. Now core 1
    // parks itself from an interrupt, and because `Spinlock::with` takes its
    // lock inside `arch::cs_with`, an interrupt cannot land here while a lock
    // is held. A core parked this way provably holds nothing.
    //
    // Routed in the **APP** crossbar table only, so raising `FROM_CPU_2`
    // interrupts this core and no other. Registered as IRAM-safe because that
    // is exactly what it is: the handler's whole job is to run with the cache
    // off. Failure is reported and not fatal — the flash driver falls back to
    // the stall it used to do, which is worse but works.
    #[cfg(target_os = "none")]
    unsafe {
        use soc_esp32::crosscore::Signal;
        use soc_esp32::intr_map::{self, Core};

        const PARK_CPU_INT: u8 = 5;
        match intr_map::route_on(Core::App, Signal::FromCpu2.source(), PARK_CPU_INT) {
            Ok(()) => {
                if crate::interrupt::register_iram_safe(PARK_CPU_INT, park_isr) {
                    registers::enable_interrupt(PARK_CPU_INT as u32);
                } else {
                    debug::fault::raw_print(
                        "[FLINT] WARNING: no slot for the flash park handler; \
                         flash will stall core 1\r\n",
                    );
                }
            }
            Err(_) => debug::fault::raw_print(
                "[FLINT] WARNING: could not route the flash park signal; \
                 flash will stall core 1\r\n",
            ),
        }
    }

    let _prev = registers::set_intlevel_0();

    idle_loop()
}
