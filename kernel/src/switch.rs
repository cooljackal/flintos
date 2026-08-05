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
use core::sync::atomic::{AtomicBool, Ordering};

use crate::scheduler::{self, TaskState};
use crate::{interrupt, timer, debug};

// One-shot bring-up markers. Whether the timer interrupt fires at all, and
// whether a context switch ever happens, are otherwise unobservable from the
// console: a kernel that never schedules and a kernel whose timer never ticks
// produce byte-identical silence.
static FIRST_TRAP: AtomicBool = AtomicBool::new(false);
static FIRST_TICK: AtomicBool = AtomicBool::new(false);
static FIRST_SWITCH: AtomicBool = AtomicBool::new(false);

fn announce_once(flag: &AtomicBool, msg: &str) {
    if !flag.swap(true, Ordering::Relaxed) {
        debug::fault::raw_print(msg);
    }
}

/// Called from `_flint_trap_entry` in vectors.S.
///
/// # Safety
/// `frame` must point to a valid `TaskContext`-layout frame built by the trap
/// entry stub.
#[no_mangle]
pub extern "C" fn _flint_trap(frame: *mut TaskContext) -> *mut TaskContext {
    let cause = unsafe { registers::read_exccause() };
    announce_once(&FIRST_TRAP, "[FLINT] first trap serviced\r\n");

    if cause == registers::EXCCAUSE_LEVEL1_INTERRUPT {
        let pending = unsafe { registers::read_interrupt() & registers::read_intenable() };

        // Timer tick.
        if pending & registers::INT_TIMER0_MASK != 0 {
            XtensaTick::tick(); // ack + re-arm + advance counter
            announce_once(&FIRST_TICK, "[FLINT] first timer tick\r\n");
            let now = XtensaTick::now();

            // Bring-up heartbeat. Truncated console output has two completely
            // different causes that look identical from outside: the kernel
            // died, or the kernel is fine and the running task is wedged. This
            // separates them, and reports where the interrupted task actually
            // is so a hang can be located rather than guessed at.
            // The first few ticks are printed unconditionally: the failures
            // seen so far all occur within the first two or three, so a
            // heartbeat that only starts at 500 reports nothing at all.
            if now <= 6 || now % 500 == 0 {
                let cur = scheduler::global().current;
                let sp = unsafe { (*frame).a[1] };
                let (base, size) = scheduler::global().tasks[cur as usize]
                    .as_ref()
                    .map_or((0, 0), |t| (t.stack_base, t.stack_size));

                debug::fault::raw_print("[FLINT] tick=");
                debug::fault::raw_dec(now as u32);
                debug::fault::raw_print(" cur=");
                debug::fault::raw_dec(cur);
                debug::fault::raw_print(" pc=0x");
                debug::fault::raw_hex(unsafe { (*frame).pc });
                debug::fault::raw_print(" ws=0x");
                debug::fault::raw_hex(unsafe { (*frame).windowstart });
                debug::fault::raw_print(" sp=0x");
                debug::fault::raw_hex(sp);
                // Headroom left below the interrupted stack pointer. This is
                // the number that decides whether the trap path is overrunning
                // the task it interrupted: the frame alone is 288 bytes and
                // _flint_trap reserves 272 more on top of it.
                debug::fault::raw_print(" free=");
                // stack_size 0 means the task runs on the boot stack (idle),
                // where there is no pool to measure against.
                if size == 0 {
                    debug::fault::raw_print("n/a");
                } else {
                    debug::fault::raw_dec(sp.saturating_sub(base));
                    debug::fault::raw_print("/");
                    debug::fault::raw_dec(size);
                }
                debug::fault::raw_print("\r\n");

                // On the first few ticks, dump the interrupted window. When a
                // task is wedged on its own `entry`, the question is which
                // register the window overflow handler is using as its spill
                // base and what that register actually holds -- and that has
                // now been guessed wrong twice. Print it instead.
                if now <= 3 {
                    debug::fault::raw_print("        wb=0x");
                    debug::fault::raw_hex(unsafe { (*frame).windowbase });
                    debug::fault::raw_print(" ps=0x");
                    debug::fault::raw_hex(unsafe { (*frame).ps });
                    debug::fault::raw_print("\r\n        a:");
                    for i in 0..16 {
                        debug::fault::raw_print(" ");
                        debug::fault::raw_hex(unsafe { (*frame).a[i] });
                        if i == 7 {
                            debug::fault::raw_print("\r\n          ");
                        }
                    }
                    debug::fault::raw_print("\r\n");
                }
            }

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
            announce_once(&FIRST_SWITCH, "[FLINT] first context switch\r\n");
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
