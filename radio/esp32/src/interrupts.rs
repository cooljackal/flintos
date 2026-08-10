// SPDX-License-Identifier: Apache-2.0

//! `_set_intr`, `_clear_intr` and `_set_isr` — the blob's route to an
//! interrupt.
//!
//! Three entries in `wifi_osi_funcs_t`, and between them the reason the radio
//! cannot deliver a packet without this file. esp-idf implements them as
//! `intr_matrix_set` and `xt_set_interrupt_handler`; FlintOS already has both
//! halves — `soc_esp32::intr_map` for the crossbar and `kernel::interrupt` for
//! the CPU-side mask and dispatch — so what is left is the shape mismatch
//! between them.
//!
//! # The mismatch, and the trampolines
//!
//! The blob hands over `void (*f)(void *arg)` and expects its `arg` back.
//! `kernel::interrupt::register` takes a plain `fn()`, which carries no data,
//! and that is deliberate: a top-half runs in trap context where it cannot
//! take a lock, so the kernel gives it nothing that would need one.
//!
//! A closure cannot bridge that — `fn()` has no environment. What can is a
//! *distinct function per CPU interrupt*, each knowing its own number at
//! compile time. `trampoline::<N>` is a separate function item for every `N`,
//! so [`TRAMPOLINES`] is 32 different `fn()` values, each of which looks up
//! slot `N` and calls what the blob installed there.
//!
//! That costs one static array and no allocation, and it keeps the kernel's
//! contract intact rather than widening `register` to take a `*mut c_void`
//! that only this crate would ever use.

use core::ffi::c_void;

use kernel::interrupt;

/// CPU interrupt inputs, matching `kernel::interrupt`'s table size.
const CPU_INTS: usize = 32;

/// What the blob installed for one CPU interrupt.
///
/// `*mut c_void` is not `Send`, and this is written from task context and read
/// from trap context, so it is held as a `usize` and cast at the point of
/// call. The pointer is the blob's own and outlives the handler by its own
/// construction; nothing here dereferences it.
#[derive(Clone, Copy)]
struct Installed {
    handler: Option<unsafe extern "C" fn(*mut c_void)>,
    arg: usize,
}

impl Installed {
    const NONE: Self = Installed { handler: None, arg: 0 };
}

/// The installed handlers.
///
/// Behind the kernel's `Spinlock` rather than a bare `static mut`: written
/// from whichever core runs `_set_isr` and read from trap context on either,
/// which is exactly the case a critical section does not cover — `rsil` masks
/// the calling core alone.
static INSTALLED: kernel::smp::Spinlock<[Installed; CPU_INTS]> =
    kernel::smp::Spinlock::new([Installed::NONE; CPU_INTS]);

/// The trap-context half for CPU interrupt `N`.
///
/// Reads the slot with `try_with`, not `with`. A top-half that spins on a lock
/// held by a task it interrupted deadlocks that core, and `_set_isr` holds
/// this lock for the length of one array write — so the losing case is a
/// single missed interrupt during installation, against a hang.
fn trampoline<const N: usize>() {
    let slot = INSTALLED.try_with(|t| t[N]);
    if let Some(Installed { handler: Some(f), arg }) = slot {
        unsafe { f(arg as *mut c_void) }
    }
}

/// One distinct `fn()` per CPU interrupt, which is the whole trick.
static TRAMPOLINES: [fn(); CPU_INTS] = [
    trampoline::<0>, trampoline::<1>, trampoline::<2>, trampoline::<3>,
    trampoline::<4>, trampoline::<5>, trampoline::<6>, trampoline::<7>,
    trampoline::<8>, trampoline::<9>, trampoline::<10>, trampoline::<11>,
    trampoline::<12>, trampoline::<13>, trampoline::<14>, trampoline::<15>,
    trampoline::<16>, trampoline::<17>, trampoline::<18>, trampoline::<19>,
    trampoline::<20>, trampoline::<21>, trampoline::<22>, trampoline::<23>,
    trampoline::<24>, trampoline::<25>, trampoline::<26>, trampoline::<27>,
    trampoline::<28>, trampoline::<29>, trampoline::<30>, trampoline::<31>,
];

/// `_set_isr(n, f, arg)` — install `f` as the handler for CPU interrupt `n`.
///
/// esp-idf's `xt_set_interrupt_handler`. Installing is separate from routing:
/// the blob calls this first and `_set_intr` after, or the other way round,
/// and neither order may lose the other's work. So this only records the
/// handler, and [`set_intr`] does the routing and unmasking.
///
/// # Safety
/// `f` is called in trap context with `arg`. Called by the blob.
#[no_mangle]
pub unsafe extern "C" fn set_isr(n: i32, f: *mut c_void, arg: *mut c_void) {
    let Ok(idx) = usize::try_from(n) else {
        api::log_error!("radio: _set_isr with a negative interrupt number {}", n);
        return;
    };
    if idx >= CPU_INTS {
        api::log_error!("radio: _set_isr for CPU interrupt {}, past the {} the kernel has", idx, CPU_INTS);
        return;
    }
    // A null `f` is how esp-idf's callers clear a handler.
    let handler = if f.is_null() {
        None
    } else {
        Some(unsafe { core::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void)>(f) })
    };
    INSTALLED.with(|t| t[idx] = Installed { handler, arg: arg as usize });
}

/// `_set_intr(cpu, source, num, prio)` — route peripheral `source` to CPU
/// interrupt `num` and unmask it.
///
/// esp-idf's `intr_matrix_set`. `prio` is accepted and ignored: on this chip
/// the priority is a property of the CPU interrupt number, not something set
/// alongside it, so the blob's choice of `num` has already decided it. Saying
/// so here beats silently taking an argument that cannot be honoured.
///
/// `cpu` is checked rather than ignored. The blob asks for the core it is
/// running on, and routing a radio interrupt to the other core would deliver
/// it somewhere the blob does not expect.
///
/// # Safety
/// Writes the interrupt crossbar and unmasks a CPU input. Called by the blob.
#[no_mangle]
pub unsafe extern "C" fn set_intr(cpu: i32, source: u32, num: u32, _prio: i32) {
    let (Ok(source), Ok(num)) = (u8::try_from(source), u8::try_from(num)) else {
        api::log_error!("radio: _set_intr source={} num={} is out of range", source, num);
        return;
    };
    if num as usize >= CPU_INTS {
        api::log_error!("radio: _set_intr onto CPU interrupt {}, past the {} the kernel has", num, CPU_INTS);
        return;
    }
    let me = kernel::smp::current_core().0 as i32;
    if cpu != me {
        api::log_error!(
            "radio: _set_intr asked for core {} while running on core {}; \
             routing there would deliver the interrupt to the wrong core",
            cpu,
            me
        );
        return;
    }
    if let Err(e) = unsafe { interrupt::connect(source, num, TRAMPOLINES[num as usize]) } {
        api::log_error!("radio: _set_intr could not route source {} to CPU interrupt {}: {:?}", source, num, e);
    }
}

/// `_clear_intr(source, num)`.
///
/// esp-idf's own table leaves this pointing at a function that does nothing on
/// the ESP32 — the crossbar entry is overwritten by the next `intr_matrix_set`
/// rather than torn down, and the Wi-Fi driver never unroutes. Implemented as
/// the same no-op, and named as one, because a version that *did* unroute
/// would be a behaviour esp-idf does not have and the blob has never been
/// tested against.
///
/// # Safety
/// Does nothing. Called by the blob.
#[no_mangle]
pub unsafe extern "C" fn clear_intr(_source: u32, _num: u32) {}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_is_one_trampoline_per_cpu_interrupt() {
        // The array is written out by hand, so the thing to check is that it
        // is the right length and that no entry was duplicated by a slip --
        // `trampoline::<7>` written twice would silently route two interrupts
        // to one slot.
        assert_eq!(TRAMPOLINES.len(), CPU_INTS);
        for (i, a) in TRAMPOLINES.iter().enumerate() {
            for (j, b) in TRAMPOLINES.iter().enumerate().skip(i + 1) {
                assert!(
                    !core::ptr::fn_addr_eq(*a, *b),
                    "CPU interrupts {i} and {j} share a trampoline"
                );
            }
        }
    }

    #[test]
    fn a_handler_round_trips_through_the_table() {
        static SEEN: core::sync::atomic::AtomicUsize =
            core::sync::atomic::AtomicUsize::new(0);
        unsafe extern "C" fn h(arg: *mut c_void) {
            SEEN.store(arg as usize, core::sync::atomic::Ordering::SeqCst);
        }
        unsafe { set_isr(3, h as *mut c_void, 0xABCD as *mut c_void) };
        trampoline::<3>();
        assert_eq!(SEEN.load(core::sync::atomic::Ordering::SeqCst), 0xABCD);

        // And a slot nobody installed does nothing rather than jumping to zero.
        trampoline::<4>();
        assert_eq!(SEEN.load(core::sync::atomic::Ordering::SeqCst), 0xABCD);
    }

    #[test]
    fn a_null_handler_clears_the_slot() {
        static FIRED: core::sync::atomic::AtomicBool =
            core::sync::atomic::AtomicBool::new(false);
        unsafe extern "C" fn h(_: *mut c_void) {
            FIRED.store(true, core::sync::atomic::Ordering::SeqCst);
        }
        unsafe { set_isr(5, h as *mut c_void, core::ptr::null_mut()) };
        unsafe { set_isr(5, core::ptr::null_mut(), core::ptr::null_mut()) };
        trampoline::<5>();
        assert!(!FIRED.load(core::sync::atomic::Ordering::SeqCst), "cleared slot still fired");
    }

    #[test]
    fn an_out_of_range_interrupt_is_refused_not_wrapped() {
        // The failure this prevents: an index past the kernel's table, which
        // as an array write would corrupt whatever follows it.
        unsafe { set_isr(CPU_INTS as i32, core::ptr::null_mut(), core::ptr::null_mut()) };
        unsafe { set_isr(-1, core::ptr::null_mut(), core::ptr::null_mut()) };
        // Nothing to assert beyond not panicking: the point is that neither
        // call touched the table.
    }
}
