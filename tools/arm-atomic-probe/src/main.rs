// SPDX-License-Identifier: Apache-2.0

#![no_main]
#![no_std]

use core::arch::asm;
use core::panic::PanicInfo;
use portable_atomic::{AtomicU32, AtomicU8, AtomicUsize, Ordering};

const SIO_SPINLOCK_14: *mut u32 = (0xd000_0000usize + 0x100 + 14 * 4) as *mut u32;

struct Rp2040CriticalSection;

critical_section::set_impl!(Rp2040CriticalSection);

unsafe impl critical_section::Impl for Rp2040CriticalSection {
    unsafe fn acquire() -> critical_section::RawRestoreState {
        let primask: u32;
        asm!("mrs {state}, PRIMASK", "cpsid i", state = out(reg) primask, options(nomem, nostack));
        while core::ptr::read_volatile(SIO_SPINLOCK_14) == 0 {}
        primask
    }

    unsafe fn release(primask: critical_section::RawRestoreState) {
        core::ptr::write_volatile(SIO_SPINLOCK_14, 1);
        asm!("msr PRIMASK, {state}", state = in(reg) primask, options(nomem, nostack));
    }
}

static BYTE: AtomicU8 = AtomicU8::new(1);
static WORD: AtomicU32 = AtomicU32::new(2);
static POINTER: AtomicUsize = AtomicUsize::new(3);

#[no_mangle]
#[link_section = ".text.reset"]
pub extern "C" fn reset() -> ! {
    let _ = BYTE.compare_exchange(1, 4, Ordering::SeqCst, Ordering::SeqCst);
    let _ = WORD.compare_exchange(2, 5, Ordering::SeqCst, Ordering::SeqCst);
    let _ = POINTER.fetch_add(6, Ordering::SeqCst);
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
