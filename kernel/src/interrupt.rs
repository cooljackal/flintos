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

use flint_arch_xtensa::cs_with;
use flint_arch_xtensa::registers;

const MAX_HANDLERS: usize = 32;

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

/// Register a top-half for `irq`. Returns false if the table is full.
pub fn register(irq: u8, isr: fn()) -> bool {
    cs_with(|| {
        let h = handlers();
        for slot in h.iter_mut() {
            if slot.is_none() {
                *slot = Some(Handler { irq, isr });
                return true;
            }
        }
        false
    })
}

/// Dispatch a fired IRQ to its top-half. Called from the trap handler with
/// interrupts already masked, so no extra critical section is taken.
pub fn dispatch(irq: u8) {
    let h = handlers();
    for slot in h.iter() {
        if let Some(handler) = slot {
            if handler.irq == irq {
                (handler.isr)();
                break;
            }
        }
    }
    clear_pending(irq);
}

/// Acknowledge a CPU-level edge/software interrupt via the INTCLEAR special
/// register. Level-triggered peripheral sources must additionally be cleared at
/// the peripheral by the driver's top-half.
pub fn clear_pending(irq: u8) {
    unsafe { registers::intclear(1u32 << irq) };
}
