// SPDX-License-Identifier: Apache-2.0

//! The APP CPU's entry point and its stack.
//!
//! The trampoline lives in `asm/appcpu.S`; this provides the two things it
//! needs from Rust — somewhere to put a stack pointer, and a function to call.

use core::sync::atomic::{AtomicU32, Ordering};

extern "C" {
    /// The assembly trampoline. Hand this to `soc_esp32::appcpu::start`.
    pub fn _flint_appcpu_entry() -> !;
}

/// The APP CPU's stack.
///
/// 4 KiB, and it is all that core gets. That budget was set when this core
/// only ran a bare `main`; it no longer does. `kernel::boot::join_scheduler`
/// makes it a full peer — it takes traps and ticks, and its idle task is
/// pinned so that idle runs on *this* array. So the 4 KiB has to cover the
/// deepest call chain plus a trap frame plus the kernel's trap handler on
/// top, exactly as `apps/README.md` says of any task stack.
///
/// 16-byte aligned because the Xtensa ABI requires it of any stack pointer,
/// and a misaligned one faults on the first windowed call rather than at the
/// misalignment.
/// The APP CPU's stack size. See the note above on why 4 KiB.
const APPCPU_STACK_BYTES: usize = 4096;

#[repr(C, align(16))]
struct Stack([u8; APPCPU_STACK_BYTES]);

static mut APPCPU_STACK: Stack = Stack([0; APPCPU_STACK_BYTES]);

/// The top of that stack, as a word the trampoline can load.
///
/// Exported as a *pointer to a value* rather than as a symbol at the top of
/// the array, because `movi` on Xtensa loads a symbol's address, and the
/// address of the array is its bottom. The trampoline loads this word.
#[no_mangle]
pub static _flint_appcpu_stack_top: AtomicU32 = AtomicU32::new(0);

/// What the APP CPU runs, once started.
static APPCPU_MAIN: AtomicU32 = AtomicU32::new(0);

/// Prepare the trampoline's stack pointer and the function it will call.
///
/// Call before `soc_esp32::appcpu::start`.
///
/// # Safety
/// `main` must never return. It *may* call `kernel::boot::join_scheduler`,
/// which is how this core becomes a scheduling peer; until it does, the core
/// has no vector table, so a fault there is unreportable rather than
/// diagnosable — see `soc_esp32::appcpu::start`.
///
/// **`main` must be in IRAM**, as must everything it calls. The APP CPU starts
/// with no instruction cache, so anything mapped from flash faults on the
/// first fetch — on a core with no vector table to report it. Mark it
/// `#[link_section = ".iram1.<name>"]`.
pub unsafe fn prepare(main: extern "C" fn() -> !) {
    let base = core::ptr::addr_of!(APPCPU_STACK) as u32;
    // Stack grows down, so start at the top. The Xtensa ABI also wants 16
    // bytes of headroom below the initial SP for a call's save area.
    let top = (base + APPCPU_STACK_BYTES as u32 - 16) & !0xF;
    _flint_appcpu_stack_top.store(top, Ordering::SeqCst);
    APPCPU_MAIN.store(main as usize as u32, Ordering::SeqCst);
}

/// Called by the trampoline with a usable stack.
///
/// # Safety
/// Called from assembly on the second core.
/// In IRAM for the same reason as the trampoline: this runs before the APP
/// CPU's cache exists, so it cannot be fetched from flash.
#[link_section = ".iram1.appcpu_main"]
#[no_mangle]
pub unsafe extern "C" fn flint_appcpu_main() -> ! {
    let f = APPCPU_MAIN.load(Ordering::SeqCst);
    if f != 0 {
        let entry: extern "C" fn() -> ! = core::mem::transmute(f as usize);
        entry();
    }
    // Nothing to run. Park rather than execute whatever is at address zero.
    loop {
        core::hint::spin_loop();
    }
}
