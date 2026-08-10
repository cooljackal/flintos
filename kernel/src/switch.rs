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

use hal::tick::TickSource;
use hal::types::TaskContext;
use crate::arch::registers;
use crate::arch::Tick;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::scheduler::{self, TaskState};
use crate::{interrupt, timer, debug};

/// Emit the trap-path bring-up diagnostics: one-shot markers for the first
/// trap, tick and context switch, plus a heartbeat every 1000 ticks.
///
/// Off by default, because on a working kernel it is pure noise. Turn it on
/// when the console goes quiet: a kernel that never schedules and a kernel
/// whose timer never ticks produce byte-identical silence, and truncated output
/// looks the same whether the kernel died or the running task wedged. The
/// heartbeat separates those and reports where the interrupted task actually
/// is, so a hang can be located rather than guessed at.
///
/// This is what found the register-window fault in the trap entry (issue #1).
pub const TRAP_DIAGNOSTICS: bool = false;

static FIRST_TRAP: AtomicBool = AtomicBool::new(false);
static FIRST_TICK: AtomicBool = AtomicBool::new(false);
static FIRST_SWITCH: AtomicBool = AtomicBool::new(false);

fn announce_once(flag: &AtomicBool, msg: &str) {
    if TRAP_DIAGNOSTICS && !flag.swap(true, Ordering::Relaxed) {
        debug::fault::raw_print(msg);
    }
}

/// Called from `_flint_trap_entry` in vectors.S.
///
/// Register windows are already spilled by the time this runs: the trap entry
/// does it in assembly, before it moves the stack pointer. It cannot be done
/// from here -- a spill driven from Rust runs with a1 already lowered by the
/// size of the trap frame, so the overflow handlers write each caller's
/// registers 112 bytes below where the matching underflow will read them.
///
/// # Safety
/// `frame` must point to a valid `TaskContext`-layout frame built by the trap
/// entry stub. Marked `unsafe` to match that contract rather than only
/// describing it: the only caller is `vectors.S`, so nothing in Rust has to
/// change, and the symbol `#[no_mangle]` emits is unaffected.
#[no_mangle]
pub unsafe extern "C" fn _flint_trap(frame: *mut TaskContext) -> *mut TaskContext {
    let cause = unsafe { registers::read_exccause() };
    announce_once(&FIRST_TRAP, "[FLINT] first trap serviced\r\n");

    if cause == registers::EXCCAUSE_LEVEL1_INTERRUPT {
        let pending = unsafe { registers::read_interrupt() & registers::read_intenable() };

        // Timer tick.
        if pending & registers::INT_TIMER0_MASK != 0 {
            Tick::tick(); // ack + re-arm + advance counter

            // Proof of life for the RTC watchdog. Here rather than anywhere in
            // task context: this runs if and only if the kernel is still
            // servicing interrupts, which is exactly the property it attests.
            crate::watchdog::feed_from_tick();
            announce_once(&FIRST_TICK, "[FLINT] first timer tick\r\n");
            let now = Tick::now();

            if TRAP_DIAGNOSTICS && now % 1000 == 0 {
                scheduler::with(|sched| {
                    let cur = sched.current();
                    debug::fault::raw_print("[FLINT] core=");
                    debug::fault::raw_dec(crate::smp::current_core().0 as u32);
                    debug::fault::raw_print(" t=");
                    debug::fault::raw_dec(now as u32);
                    debug::fault::raw_print(" cur=");
                    debug::fault::raw_dec(cur);
                    debug::fault::raw_print(":");
                    debug::fault::raw_print(match &sched.tasks[cur as usize] {
                        Some(tcb) => tcb.name,
                        None => "?",
                    });
                    // Affinity, because "why is this task here" is the
                    // question a pinned system makes you ask.
                    debug::fault::raw_print(match sched.tasks[cur as usize]
                        .as_ref()
                        .map(|t| t.affinity)
                    {
                        Some(scheduler::Affinity::Any) => " aff=any",
                        Some(scheduler::Affinity::Core(c)) if c.0 == 0 => " aff=core0",
                        Some(scheduler::Affinity::Core(_)) => " aff=core1",
                        None => " aff=?",
                    });
                    debug::fault::raw_print(" ready=");
                    debug::fault::raw_hex(sched.ready_mask as u32);
                });
                debug::fault::raw_print(" pc=");
                debug::fault::raw_hex(unsafe { (*frame).pc });
                debug::fault::raw_print(" ws=");
                debug::fault::raw_hex(unsafe { (*frame).windowstart });
                debug::fault::raw_print("\r\n");
            }

            // One lock for the whole tick update, not one per read: the other
            // core must never observe `on_tick` half applied.
            scheduler::with(|sched| {
                if sched.on_tick(now) {
                    scheduler::set_pending_switch();
                }
                // Stack high-water for the running task, while we hold it.
                let cur = sched.current();
                debug::stack::update_hwm(sched, cur);
            });
            timer::process_timers(now);
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
    //
    // Under the scheduler lock for the whole decision, not just parts of it:
    // choosing a task, saving the outgoing context and installing the incoming
    // one have to look atomic to the other core, or it can schedule the same
    // task twice.
    //
    // The returned pointer outlives the lock, which is sound for the reason
    // the trap handler is special: it returns straight into the assembly that
    // restores from that frame, on this core, with interrupts still masked.
    let next_frame = scheduler::with(|sched| {
        if !scheduler::take_pending_switch() {
            return None;
        }
        let cur = sched.current();
        let next = sched.schedule();
        if next == cur {
            return None;
        }
        announce_once(&FIRST_SWITCH, "[FLINT] first context switch\r\n");
        // Save the interrupted context into the current task's TCB, unless it
        // has been torn down.
        if let Some(tcb) = &mut sched.tasks[cur as usize] {
            // The current task was Running; demote to Ready unless it blocked
            // itself (block_current already set a blocked state).
            if tcb.state == TaskState::Running {
                tcb.state = TaskState::Ready;
                let prio = tcb.priority;
                sched.ready_mask |= 1u64 << prio;
            }
            unsafe { core::ptr::copy_nonoverlapping(frame, &mut tcb.context, 1) };
        }
        sched.set_current(next);
        sched.tasks[next as usize]
            .as_mut()
            .map(|tcb| &mut tcb.context as *mut TaskContext)
    });

    next_frame.unwrap_or(frame)
}
