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

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
    /// Whether this handler may run with the instruction cache disabled.
    ///
    /// False by default, and that default is the safe one: a handler that
    /// says yes and is not is a core that stops dead mid-flash-write, with
    /// nothing to show for it. See [`register_iram_safe`].
    iram_safe: bool,
}

/// The handler table.
///
/// Deliberately **not** behind a `Spinlock`, unlike `queue`'s and `timer`'s.
/// Two of the three readers cannot take one:
///
/// - `mask_non_iram_safe` runs immediately before the instruction cache is
///   switched off. A spinlock spins until the other core releases it, and the
///   other core is about to be stalled by the same flash operation — so a
///   contended lock here is not a delay, it is a board that never comes back.
/// - `dispatch` runs in trap context on either core, on the hot path.
///
/// What makes the reads sound is *when* the writes happen: `register` and
/// `register_iram_safe` are init-time calls, made before the second core is
/// brought up, and the table is read-only afterwards. Today nothing in the
/// tree registers at all outside the self-tests.
///
/// **This is the one static here whose safety is a timing argument rather than
/// a lock**, so it is the one to revisit first if a driver ever registers a
/// handler at runtime. The fix then is not a lock on the read path — it is to
/// make the table append-only with atomic slots, so a reader either sees a
/// complete entry or an empty one.
static mut HANDLERS: [Option<Handler>; MAX_HANDLERS] = [None; MAX_HANDLERS];

/// In IRAM, because two of its callers run with the instruction cache off.
///
/// It is three instructions and the optimiser would usually fold it away, but
/// "usually" is not a placement guarantee — `#[link_section]` says where a
/// body goes and nothing about a copy inlined into a caller, and at
/// `opt-level = 1` across a crate boundary the call is real. A flash-resident
/// copy of even this much is a core that stops fetching.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.intr")]
fn handlers() -> &'static mut [Option<Handler>; MAX_HANDLERS] {
    unsafe { &mut *core::ptr::addr_of_mut!(HANDLERS) }
}

/// Register a top-half for `irq`. Returns false if the table is full, or if
/// `irq` already has a registered handler — a second registration would
/// otherwise be silently unreachable dead code (`dispatch` stops at the
/// first match), which is exactly the "plausible-looking wrong result" the
/// project's error-handling philosophy forbids (item 10a).
pub fn register(irq: u8, isr: fn()) -> bool {
    register_inner(irq, isr, false)
}

/// Register a top-half that may run while the instruction cache is off.
///
/// A flash erase or program runs with the cache disabled, and anything fetched
/// from flash during that window fetches nothing at all — the core stops. So
/// FlintOS masks interrupts for the duration. Blanket masking is safe but
/// costs the whole operation in latency, and a sector erase is tens of
/// milliseconds: long enough to drop a Wi-Fi link.
///
/// This is the opt-out. A handler registered here stays enabled through a
/// flash operation, and in exchange it promises that **it and everything it
/// calls** live in IRAM. esp-idf spells the same promise `ESP_INTR_FLAG_IRAM`
/// and NuttX honours it in `esp32_spiflash_opstart`.
///
/// # The promise is not checked
///
/// Nothing here can verify it. `#[link_section]` places a function body and
/// says nothing about a copy the optimiser folded into a caller — a trap this
/// project has fallen into three times in the flash driver alone. Check the
/// built ELF, not the attributes.
///
/// Breaking it does not fault. The core simply stops fetching, which presents
/// as a board that went silent during a flash write, nowhere near the handler
/// that caused it.
///
/// # And it must not take a lock
///
/// A second promise, and a less obvious one. The other core is **stalled in
/// hardware** for the duration of a flash operation — see `esp32-flash`'s
/// module docs — and a stalled core does not release what it holds. A handler
/// that waits on a `Spinlock` core 1 was stalled holding will wait for the
/// rest of time.
///
/// That constraint used to be satisfied by accident, because nothing ran
/// between the stall and the release. It is a real constraint now: this is the
/// register for handlers that run in exactly that window. `Queue::send_isr`
/// and the atomics are fine; anything that locks is not.
///
/// The way out, when a handler genuinely needs one, is NuttX's
/// `esp32_spiflash_opstart`: park the other core with a handshake it enters
/// voluntarily, at a point where it holds nothing, instead of stalling it
/// wherever it happens to be.
pub fn register_iram_safe(irq: u8, isr: fn()) -> bool {
    register_inner(irq, isr, true)
}

fn register_inner(irq: u8, isr: fn(), iram_safe: bool) -> bool {
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
                *slot = Some(Handler { irq, isr, iram_safe });
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

// ── Bringing a peripheral's interrupt all the way to a handler ──────────────

/// Why [`connect`] could not deliver a peripheral's interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectError {
    /// The crossbar refused the pairing — no such source, or a CPU input the
    /// kernel could not service.
    Route,
    /// That CPU input already has a handler. Deliberately not silent: a second
    /// registration would be unreachable, because `dispatch` stops at the
    /// first match.
    AlreadyRegistered,
}

/// Route a peripheral source to a CPU input, take the handler, and unmask it.
///
/// Three steps that must happen in this order, and did not have one place to
/// live: six call sites retyped them, which is how one of them eventually gets
/// it wrong. The order is load-bearing in both directions —
///
/// - unmasking before a handler is registered leaves a window where the
///   interrupt fires and `dispatch` finds nothing, and for a level-triggered
///   source that window does not close: it re-enters forever;
/// - registering before routing is harmless but useless, and reads as though
///   the route were optional.
///
/// Enabling the interrupt *at the peripheral* is still the driver's job, and
/// deliberately separate. A peripheral that raises its line before the kernel
/// is ready to answer is the same storm by a different route.
///
/// # Safety
/// Writes the interrupt crossbar and unmasks a CPU input. `handler` runs in
/// trap context: it must be short, must not block, and must acknowledge its
/// peripheral.
pub unsafe fn connect(source: u8, cpu_int: u8, handler: fn()) -> Result<(), ConnectError> {
    unsafe { connect_inner(source, cpu_int, handler, false) }
}

/// [`connect`], for a handler that has made the promises in
/// [`register_iram_safe`] — IRAM, and no locks.
///
/// # Safety
/// As [`connect`], plus both of those promises.
pub unsafe fn connect_iram_safe(
    source: u8,
    cpu_int: u8,
    handler: fn(),
) -> Result<(), ConnectError> {
    unsafe { connect_inner(source, cpu_int, handler, true) }
}

unsafe fn connect_inner(
    source: u8,
    cpu_int: u8,
    handler: fn(),
    iram_safe: bool,
) -> Result<(), ConnectError> {
    route(source, cpu_int)?;
    if !register_inner(cpu_int, handler, iram_safe) {
        return Err(ConnectError::AlreadyRegistered);
    }
    unmask(cpu_int);
    Ok(())
}

// The crossbar is the SoC's and the mask is the core's, so the two halves come
// from different crates and neither exists on a host. Split here rather than in
// `arch`, which has no business naming a chip.
#[cfg(target_os = "none")]
unsafe fn route(source: u8, cpu_int: u8) -> Result<(), ConnectError> {
    soc_esp32::intr_map::route(source, cpu_int).map_err(|_| ConnectError::Route)
}

#[cfg(not(target_os = "none"))]
unsafe fn route(_source: u8, _cpu_int: u8) -> Result<(), ConnectError> {
    Ok(())
}

#[cfg(target_os = "none")]
unsafe fn unmask(cpu_int: u8) {
    registers::enable_interrupt(cpu_int as u32);
}

#[cfg(not(target_os = "none"))]
unsafe fn unmask(_cpu_int: u8) {}

// ── Masking for flash operations ────────────────────────────────────────────

/// Set for as long as the instruction cache is off for a flash operation.
///
/// The trap handler reads this **first**, before anything that might live in
/// flash, and takes a different path when it is set. Nothing else can tell:
/// a trap taken inside that window looks exactly like any other one, and the
/// difference between the two paths is the difference between servicing an
/// interrupt and stopping the core.
///
/// Owned by [`mask_non_iram_safe`]/[`restore_mask`] because those two are the
/// window: the flash driver calls them as its first and last acts, so there is
/// no way to open the window without setting this and no way to close it
/// without clearing it. A separate hook would be a third thing to keep in step
/// with the other two.
static CACHE_OFF: AtomicBool = AtomicBool::new(false);

/// Whether a flash operation currently has the instruction cache disabled.
///
/// `Relaxed` is right: the only writer is the core that is about to do the
/// flash operation, and the only reader that matters is that same core's trap
/// handler. The other core is stalled in hardware for the duration and cannot
/// observe anything at all.
#[inline(always)]
pub fn cache_is_off() -> bool {
    CACHE_OFF.load(Ordering::Relaxed)
}

/// Service the interrupts that may run while the cache is off, and only those.
///
/// Everything the ordinary trap path does — the tick, the scheduler, software
/// timers, logging — lives in flash and would stop the core dead. So this does
/// none of it. No tick means no preemption decision either, which is why the
/// caller returns the frame it was given: a context switch inside this window
/// would resume a task whose code cannot be fetched.
///
/// Takes no lock, deliberately. The other core is stalled in hardware, and if
/// it were stalled holding a lock this wanted, the wait would never end.
/// [`HANDLERS`] is readable without one; see the note on that static.
///
/// # Safety
/// Runs in trap context with the instruction cache disabled. Every handler it
/// calls must have promised IRAM via [`register_iram_safe`], and this function
/// and everything it touches must be in IRAM too.
/// Target-only: there is no cache to switch off on a host, and the stand-in
/// `registers` has no `INTERRUPT` to read. Gating the function rather than
/// inventing a stand-in keeps the host suite honest about what it covers --
/// which for this path is nothing, and the on-target
/// `an_erase_does_not_stop_an_iram_safe_interrupt` is why.
#[cfg(target_os = "none")]
#[inline(never)]
#[link_section = ".iram1.intr"]
pub unsafe fn dispatch_while_cache_off() {
    let pending = unsafe { registers::read_interrupt() & registers::read_intenable() };
    for handler in handlers().iter().flatten() {
        if handler.iram_safe && handler.irq < 32 && pending & (1u32 << handler.irq) != 0 {
            (handler.isr)();
        }
    }
}

/// Mask every interrupt that is *not* safe to run with the cache off.
///
/// Returns the previous `INTENABLE`, which [`restore_mask`] puts back. The
/// handlers registered through [`register_iram_safe`] stay enabled; everything
/// else — including any CPU interrupt with no handler at all, since an
/// unregistered one has nothing promising anything — is masked.
///
/// This replaces raising `PS.INTLEVEL`, which masked the lot. Both esp-idf and
/// NuttX mask selectively for exactly this reason: a sector erase is tens of
/// milliseconds, and stopping every interrupt for that long is a real-time
/// defect whether or not a radio is involved.
///
/// # Safety
/// Must be paired with [`restore_mask`] on the same core. Interrupts stay
/// masked until it is, and a driver whose interrupt never fires again looks
/// like a dead peripheral rather than a missing call.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.intr")]
pub unsafe fn mask_non_iram_safe() -> u32 {
    let previous = unsafe { registers::read_intenable() };
    let mut keep = 0u32;
    // Deliberately not `cs_with`: the caller is already inside a critical
    // section, and taking another would nest a lock this must not depend on
    // while the cache is about to go away.
    for handler in handlers().iter().flatten() {
        if handler.iram_safe && handler.irq < 32 {
            keep |= 1u32 << handler.irq;
        }
    }
    unsafe { registers::write_intenable(previous & keep) };
    // Last, so the window opens only once the mask that makes it survivable
    // is in place.
    CACHE_OFF.store(true, Ordering::Relaxed);
    previous
}

/// Put back what [`mask_non_iram_safe`] returned.
///
/// # Safety
/// `previous` must be the value that call returned, on this core.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.intr")]
pub unsafe fn restore_mask(previous: u32) {
    // First, so the trap handler is back on its ordinary path before the
    // interrupts that path serves are re-enabled.
    CACHE_OFF.store(false, Ordering::Relaxed);
    unsafe { registers::write_intenable(previous) };
}

/// Whether `irq` has a handler that promised to be IRAM-safe. Test support.
pub fn is_iram_safe(irq: u8) -> bool {
    cs_with(|| {
        handlers()
            .iter()
            .flatten()
            .any(|h| h.irq == irq && h.iram_safe)
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy() {}

    /// Serialise: the handler table is global and the suite runs in parallel.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn a_plain_registration_is_not_iram_safe() {
        // The default has to be the safe one. A handler that is wrongly
        // believed IRAM-safe stops the core mid-flash-write, and there is
        // nothing to see afterwards.
        let _s = serial();
        assert!(register(20, dummy));
        assert!(!is_iram_safe(20), "register must not imply IRAM-safe");
        clear_for_test(20);
    }

    #[test]
    fn an_iram_safe_registration_says_so() {
        let _s = serial();
        assert!(register_iram_safe(21, dummy));
        assert!(is_iram_safe(21));
        clear_for_test(21);
    }

    #[test]
    fn masking_keeps_only_the_iram_safe_bits() {
        let _s = serial();
        assert!(register_iram_safe(22, dummy));
        assert!(register(23, dummy));
        unsafe { registers::write_intenable(u32::MAX) };

        let previous = unsafe { mask_non_iram_safe() };
        let now = unsafe { registers::read_intenable() };

        assert_eq!(previous, u32::MAX, "the old mask must be reported back");
        assert_ne!(now & (1 << 22), 0, "an IRAM-safe handler must stay enabled");
        assert_eq!(now & (1 << 23), 0, "everything else must be masked");
        // An interrupt with no handler at all promised nothing, so it goes too.
        assert_eq!(now & (1 << 24), 0, "an unregistered interrupt must be masked");

        unsafe { restore_mask(previous) };
        assert_eq!(unsafe { registers::read_intenable() }, u32::MAX, "restore must be exact");
        clear_for_test(22);
        clear_for_test(23);
    }

    #[test]
    fn masking_with_nothing_iram_safe_masks_everything() {
        // The common case today: no handler has opted in, so this is the old
        // blanket behaviour and the change is inert until something does.
        let _s = serial();
        unsafe { registers::write_intenable(u32::MAX) };
        let previous = unsafe { mask_non_iram_safe() };
        assert_eq!(unsafe { registers::read_intenable() }, 0);
        unsafe { restore_mask(previous) };
    }

    /// Free a slot so tests do not exhaust the table or collide.
    fn clear_for_test(irq: u8) {
        cs_with(|| {
            for slot in handlers().iter_mut() {
                if slot.is_some_and(|h| h.irq == irq) {
                    *slot = None;
                }
            }
        });
    }
}
