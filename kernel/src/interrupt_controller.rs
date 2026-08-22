// SPDX-License-Identifier: Apache-2.0

//! Selected machine implementation for the portable interrupt registry.

pub(crate) trait InterruptController {
    unsafe fn route(source: u8, cpu_int: u8) -> bool;
    unsafe fn clear_pending(mask: u32);
    unsafe fn unmask(cpu_int: u8);
    #[cfg_attr(not(target_os = "none"), allow(dead_code))]
    unsafe fn pending_enabled() -> u32;
    unsafe fn enabled() -> u32;
    unsafe fn set_enabled(mask: u32);
}

#[cfg(all(target_os = "none", feature = "soc-esp32"))]
pub(crate) struct Selected;

#[cfg(all(target_os = "none", feature = "soc-esp32"))]
impl InterruptController for Selected {
    unsafe fn route(source: u8, cpu_int: u8) -> bool {
        unsafe { soc_esp32::intr_map::route(source, cpu_int).is_ok() }
    }

    unsafe fn clear_pending(mask: u32) {
        unsafe { crate::arch::registers::intclear(mask) }
    }

    unsafe fn unmask(cpu_int: u8) {
        unsafe { crate::arch::registers::enable_interrupt(cpu_int as u32) }
    }

    unsafe fn pending_enabled() -> u32 {
        unsafe {
            crate::arch::registers::read_interrupt() & crate::arch::registers::read_intenable()
        }
    }

    unsafe fn enabled() -> u32 {
        unsafe { crate::arch::registers::read_intenable() }
    }

    unsafe fn set_enabled(mask: u32) {
        unsafe { crate::arch::registers::write_intenable(mask) }
    }
}

#[cfg(not(target_os = "none"))]
pub(crate) struct Selected;

#[cfg(not(target_os = "none"))]
impl InterruptController for Selected {
    unsafe fn route(_source: u8, _cpu_int: u8) -> bool {
        true
    }

    unsafe fn clear_pending(_mask: u32) {}

    unsafe fn unmask(_cpu_int: u8) {}

    unsafe fn pending_enabled() -> u32 {
        unsafe { crate::arch::registers::read_intenable() }
    }

    unsafe fn enabled() -> u32 {
        unsafe { crate::arch::registers::read_intenable() }
    }

    unsafe fn set_enabled(mask: u32) {
        unsafe { crate::arch::registers::write_intenable(mask) }
    }
}
