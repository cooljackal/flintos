// SPDX-License-Identifier: Apache-2.0

//! RP2040-safe ARMv6-M critical section.
//!
//! PRIMASK is local to one Cortex-M0+ core. SIO spinlock 14 supplies the
//! cross-core exclusion; per-core depth makes nested entry non-reentrant.

#[cfg(target_arch = "arm")]
use core::arch::asm;

#[cfg(target_arch = "arm")]
const SIO_CPUID: *const u32 = 0xd000_0000 as *const u32;
#[cfg(target_arch = "arm")]
const SIO_SPINLOCK_14: *mut u32 = 0xd000_0138 as *mut u32;

#[cfg(target_arch = "arm")]
#[derive(Clone, Copy)]
struct LocalState {
    depth: u32,
}

#[cfg(target_arch = "arm")]
static mut LOCAL: [LocalState; 2] = [LocalState { depth: 0 }; 2];

#[cfg(target_arch = "arm")]
pub unsafe fn enter_raw() -> u32 {
    let primask: u32;
    unsafe {
        asm!(
            "mrs {state}, PRIMASK",
            "cpsid i",
            state = out(reg) primask,
            options(nomem, nostack)
        );
    }
    let core = unsafe { SIO_CPUID.read_volatile() as usize };
    let state = unsafe { &mut LOCAL[core] };
    if state.depth == 0 {
        while unsafe { SIO_SPINLOCK_14.read_volatile() } == 0 {
            core::hint::spin_loop();
        }
        unsafe { asm!("dmb", options(nostack)) };
    }
    state.depth += 1;
    primask
}

#[cfg(target_arch = "arm")]
pub unsafe fn exit_raw(primask: u32) {
    let core = unsafe { SIO_CPUID.read_volatile() as usize };
    let state = unsafe { &mut LOCAL[core] };
    state.depth -= 1;
    if state.depth == 0 {
        unsafe {
            asm!("dmb", options(nostack));
            SIO_SPINLOCK_14.write_volatile(1);
        }
    }
    unsafe {
        asm!("msr PRIMASK, {state}", state = in(reg) primask, options(nomem, nostack));
    }
}

pub fn with<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(target_arch = "arm")]
    {
        let state = unsafe { enter_raw() };
        let result = f();
        unsafe { exit_raw(state) };
        result
    }
    #[cfg(not(target_arch = "arm"))]
    {
        f()
    }
}

#[cfg(not(target_arch = "arm"))]
/// Host stand-in for entering an ARM critical section.
///
/// # Safety
/// The returned token must be passed once to [`exit_raw`].
pub unsafe fn enter_raw() -> u32 {
    0
}

#[cfg(not(target_arch = "arm"))]
/// Host stand-in for restoring a saved interrupt state.
///
/// # Safety
/// `primask` must be the token returned by the matching [`enter_raw`] call.
pub unsafe fn exit_raw(_primask: u32) {}
