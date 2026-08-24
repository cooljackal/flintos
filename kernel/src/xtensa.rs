// SPDX-License-Identifier: Apache-2.0

//! Xtensa/ESP32 kernel-side glue the arch crate cannot own.
//!
//! `arch-xtensa` depends only on `hal`, so the cross-core reschedule — which
//! needs the ESP32 DPORT "from CPU" signals (`soc_esp32::crosscore`) and the
//! kernel's interrupt plumbing — lives here, exactly as the RP2040 equivalent
//! lives in [`crate::armv6m`]. `XtensaSmp::request_reschedule` calls the
//! `_flint_xtensa_request_reschedule` hook defined below.
//!
//! Two of the four `FromCpu` signals are the reschedule channels: `FromCpu0` is
//! routed in the PRO (core 0) crossbar and `FromCpu1` in the APP (core 1) one,
//! both to the same CPU interrupt input. Raising a core's signal interrupts
//! that core and no other; its handler clears the signal and asks the local
//! scheduler to switch. `request_switch_on` publishes the pending-switch flag
//! *before* it calls this, so a masked or dropped signal still resolves at the
//! next tick — the signal only removes the up-to-one-tick latency, and is never
//! the sole reason a switch happens.

use core::sync::atomic::{AtomicBool, Ordering};

use soc_esp32::crosscore::{self, Signal};
use soc_esp32::intr_map::{self, Core};

use crate::arch::registers;
use crate::debug;
use crate::smp;

/// The CPU interrupt input both reschedule channels route to.
///
/// Fixed, and the *highest* member of `intr_map`'s usable set
/// (`[0,1,2,3,4,5,8,9,12,13,17,18]`): the dynamic driver allocator walks upward
/// from 0, so it reaches this last, and because the channel is registered at
/// core-0 boot before any driver connects, a walk that did reach it finds it
/// already taken. Park's fixed input is 5; timer is 6 and the switch software
/// interrupt is 7. This must stay distinct from all three.
const RESCHEDULE_CPU_INT: u8 = 18;

/// Set once the channels are routed, registered and enabled on the boot core.
/// Read before raising a signal, so a failed setup degrades cleanly to the
/// tick fallback rather than raising a bit nothing services.
static RESCHEDULE_READY: AtomicBool = AtomicBool::new(false);

/// The `FromCpu` signal that interrupts `core`, or `None` for a core with no
/// reschedule channel.
fn signal_for(core: hal::smp::CoreId) -> Option<Signal> {
    match core.0 {
        0 => Some(Signal::FromCpu0),
        1 => Some(Signal::FromCpu1),
        _ => None,
    }
}

/// Interrupt `core` so it runs the scheduler promptly. The hook behind
/// `XtensaSmp::request_reschedule`.
///
/// Returns `false` — leaving the caller on the tick fallback — for an
/// out-of-range core, a request aimed at the calling core (which uses the local
/// software interrupt, not this), or a channel that never came up.
#[no_mangle]
extern "C" fn _flint_xtensa_request_reschedule(core: u32) -> bool {
    if !RESCHEDULE_READY.load(Ordering::Relaxed) {
        return false;
    }
    if core >= u32::from(smp::cores()) || core == u32::from(smp::current_core().0) {
        return false;
    }
    match signal_for(hal::smp::CoreId(core as u8)) {
        Some(sig) => {
            // SAFETY: `RESCHEDULE_READY` means the signal is routed and its
            // handler registered; raising it sets a DPORT bit the target core's
            // handler clears.
            unsafe { crosscore::raise(sig) };
            true
        }
        None => false,
    }
}

/// The reschedule handler, run on the interrupted (target) core.
///
/// Clears this core's signal — the `FromCpu` sources are level-triggered, so an
/// uncleared one re-enters forever — then raises the local switch request. The
/// switch itself happens on the way out of the trap, the same path a
/// cooperative yield takes.
fn reschedule_isr() {
    if let Some(sig) = signal_for(smp::current_core()) {
        // SAFETY: writes a DPORT register, clearing this core's own signal.
        unsafe { crosscore::clear(sig) };
    }
    crate::scheduler::request_switch();
}

/// Wire the reschedule channels. Called once on core 0 during boot, before the
/// secondary core starts and before the application connects any driver
/// interrupt.
///
/// Routes `FromCpu0` in the PRO crossbar and `FromCpu1` in the APP crossbar to
/// [`RESCHEDULE_CPU_INT`], registers the shared handler, and unmasks the input
/// on this (boot) core. Each secondary core unmasks the same input for itself
/// in [`enable_this_core`]. The DPORT crossbar tables are shared memory, so core
/// 0 may write the APP entry; only the unmask is per-core. Any failure leaves
/// `RESCHEDULE_READY` false, so cross-core wakeups fall back to the tick.
pub fn setup_boot_core() {
    // SAFETY: init-time, single-core. Routing writes DPORT crossbar entries;
    // enabling unmasks the input on core 0.
    unsafe {
        if intr_map::route_on(Core::Pro, Signal::FromCpu0.source(), RESCHEDULE_CPU_INT).is_err()
            || intr_map::route_on(Core::App, Signal::FromCpu1.source(), RESCHEDULE_CPU_INT).is_err()
        {
            debug::fault::raw_print(
                "[FLINT] WARNING: could not route the reschedule signals; \
                 cross-core wakeups fall back to the tick\r\n",
            );
            return;
        }
        if !crate::interrupt::register(RESCHEDULE_CPU_INT, reschedule_isr) {
            debug::fault::raw_print(
                "[FLINT] WARNING: no slot for the reschedule handler; \
                 cross-core wakeups fall back to the tick\r\n",
            );
            return;
        }
        registers::enable_interrupt(RESCHEDULE_CPU_INT as u32);
    }
    RESCHEDULE_READY.store(true, Ordering::Relaxed);
}

/// Unmask the reschedule input on the calling secondary core.
///
/// Routing and registration were done by [`setup_boot_core`]; `INTENABLE` is
/// per-core, so each secondary core must enable the input itself. A no-op if
/// the boot-core setup did not come up.
pub fn enable_this_core() {
    if RESCHEDULE_READY.load(Ordering::Relaxed) {
        // SAFETY: the input was routed and its handler registered at boot.
        unsafe { registers::enable_interrupt(RESCHEDULE_CPU_INT as u32) };
    }
}
