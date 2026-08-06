// SPDX-License-Identifier: Apache-2.0

//! Interrupt routing framework (plan W6.2).
//!
//! A peripheral IRQ vectors into the trap handler, which calls [`dispatch`].
//! Dispatch runs the registered top-half (a tiny IRAM handler) which does the
//! minimal work of enqueuing an event to the owning driver task's queue via
//! `Queue::send_isr`. The driver task's bottom-half then runs at its own
//! priority. The top-half closure owns the forwarding — there is no shared
//! mutable event buffer (the previous `&'static mut` hand-out was UB).
//!
//! Handler-table access is guarded by a critical section.

use core::sync::atomic::{AtomicU32, Ordering};
use crate::arch::cs_with;
use crate::arch::registers;

const MAX_HANDLERS: usize = 32;

// ── Interrupt-context tracking (item 11) ────────────────────────────────────
//
// Nothing about a top-half ISR or a timer callback (`timer::process_timers`)
// looks different, call-graph-wise, from ordinary task code — both can reach
// `mutex::lock`, `queue::block_send/recv`, `timer::sleep_ms`. But those are
// blocking primitives that suspend "the current task" by mutating scheduler
// state and requesting a switch; from trap context "the current task" is
// whichever task got interrupted, not the ISR. Blocking there wedges that
// task forever (it never resumes because nothing will ever wake "the ISR").
//
// `IN_INTERRUPT_DEPTH` is a nesting counter (not just a bool) so a timer
// callback firing from inside `dispatch` — or any other nesting we haven't
// thought of — still reports "in interrupt" correctly as long as every entry
// is paired with an exit via [`InterruptGuard`].
static IN_INTERRUPT_DEPTH: AtomicU32 = AtomicU32::new(0);

/// True while executing a top-half ISR or a software-timer callback (trap
/// context). Blocking primitives in `mutex.rs`/`queue.rs`/`timer.rs` check
/// this and refuse rather than silently blocking the interrupted task.
pub fn in_interrupt() -> bool {
    IN_INTERRUPT_DEPTH.load(Ordering::Relaxed) != 0
}

/// RAII marker for "we are now running trap-context code that must not
/// block". Held around the top-half call in [`dispatch`] and around the
/// callback invocation in `timer::process_timers`.
pub struct InterruptGuard;

impl InterruptGuard {
    pub fn enter() -> Self {
        IN_INTERRUPT_DEPTH.fetch_add(1, Ordering::Relaxed);
        InterruptGuard
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        IN_INTERRUPT_DEPTH.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Event delivered from a top-half to a driver task.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InterruptEvent {
    pub irq: u8,
    pub timestamp: u64,
    pub flags: u32,
}

#[derive(Clone, Copy)]
struct Handler {
    irq: u8,
    /// Top-half: runs in the trap context, must be tiny and non-blocking.
    isr: fn(),
}

static mut HANDLERS: [Option<Handler>; MAX_HANDLERS] = [None; MAX_HANDLERS];

fn handlers() -> &'static mut [Option<Handler>; MAX_HANDLERS] {
    unsafe { &mut *core::ptr::addr_of_mut!(HANDLERS) }
}

/// Register a top-half for `irq`. Returns false if the table is full, or if
/// `irq` already has a registered handler — a second registration would
/// otherwise be silently unreachable dead code (`dispatch` stops at the
/// first match), which is exactly the "plausible-looking wrong result" the
/// project's error-handling philosophy forbids (item 10a).
pub fn register(irq: u8, isr: fn()) -> bool {
    cs_with(|| {
        let h = handlers();
        if h.iter().flatten().any(|handler| handler.irq == irq) {
            crate::debug::log::write(
                api::debug::log::Level::Error,
                &format_args!("interrupt::register: irq {} already has a handler", irq),
            );
            return false;
        }
        for slot in h.iter_mut() {
            if slot.is_none() {
                *slot = Some(Handler { irq, isr });
                return true;
            }
        }
        crate::debug::log::write(
            api::debug::log::Level::Error,
            &format_args!("interrupt::register: handler table full (irq {})", irq),
        );
        false
    })
}

/// Dispatch a fired IRQ to its top-half. Called from the trap handler with
/// interrupts already masked, so no extra critical section is taken.
pub fn dispatch(irq: u8) {
    let h = handlers();
    for handler in h.iter().flatten() {
        if handler.irq == irq {
            // Mark trap-context execution for the top-half's duration so
            // blocking primitives it (mis)calls refuse instead of
            // wedging whatever task this IRQ interrupted (item 11).
            let _guard = InterruptGuard::enter();
            (handler.isr)();
            break;
        }
    }
    clear_pending(irq);
}

/// Acknowledge a CPU-level edge/software interrupt via the INTCLEAR special
/// register. Level-triggered peripheral sources must additionally be cleared at
/// the peripheral by the driver's top-half.
///
/// `irq` must be < 32 (the INTCLEAR register is 32 bits, one bit per IRQ);
/// `1u32 << irq` for `irq >= 32` is a shift-overflow that silently produces a
/// wrong (or, in debug builds, panicking) mask instead of clearing the
/// intended interrupt (item 10b) — validate instead of trusting the caller.
pub fn clear_pending(irq: u8) {
    if irq >= 32 {
        crate::debug::log::write(
            api::debug::log::Level::Error,
            &format_args!("interrupt::clear_pending: irq {} out of range (max 31)", irq),
        );
        return;
    }
    unsafe { registers::intclear(1u32 << irq) };
}
