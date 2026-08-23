// SPDX-License-Identifier: Apache-2.0

//! Selected machine implementation for the portable interrupt registry.

pub(crate) trait InterruptController {
    unsafe fn route(source: u8, cpu_int: u8) -> bool;
    /// Whether `cpu_int` is one the kernel may hand out for a peripheral: an
    /// external, level-1 input `vectors.S` has a handler for. The allocating
    /// `interrupt::connect` walks `0..32` and takes the first of these that is
    /// free. Not `unsafe` and reads no hardware — it is a property of the SoC.
    fn usable(cpu_int: u8) -> bool;
    unsafe fn clear_pending(mask: u32);
    unsafe fn unmask(cpu_int: u8);
    #[allow(dead_code)]
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

    fn usable(cpu_int: u8) -> bool {
        // Exactly `intr_map::USABLE`: the test there pins the set to the inputs
        // this predicate reports true for.
        soc_esp32::intr_map::can_serve_peripheral(cpu_int)
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

#[cfg(feature = "soc-rp2040")]
fn rp2040_can_route(source: u8, cpu_int: u8) -> bool {
    source == cpu_int && source < soc_rp2040::NVIC_IRQ_COUNT
}

#[cfg(all(target_os = "none", feature = "soc-rp2040"))]
pub(crate) struct Selected;

#[cfg(all(target_os = "none", feature = "soc-rp2040"))]
impl InterruptController for Selected {
    unsafe fn route(source: u8, cpu_int: u8) -> bool {
        rp2040_can_route(source, cpu_int)
    }

    fn usable(cpu_int: u8) -> bool {
        // The NVIC maps each peripheral to one fixed vector, so the allocator
        // can only ever land on `cpu_int == source`; `route` is what enforces
        // that. Every existing vector is a candidate here and the route gate
        // rejects the mismatches.
        (cpu_int as usize) < soc_rp2040::NVIC_IRQ_COUNT
    }

    unsafe fn clear_pending(mask: u32) {
        const NVIC_ICPR: *mut u32 = 0xE000_E280 as *mut u32;
        unsafe { NVIC_ICPR.write_volatile(mask) }
    }

    unsafe fn unmask(cpu_int: u8) {
        const NVIC_ISER: *mut u32 = 0xE000_E100 as *mut u32;
        unsafe { NVIC_ISER.write_volatile(1u32 << cpu_int) }
    }

    unsafe fn pending_enabled() -> u32 {
        const NVIC_ISER: *const u32 = 0xE000_E100 as *const u32;
        const NVIC_ISPR: *const u32 = 0xE000_E200 as *const u32;
        unsafe { NVIC_ISER.read_volatile() & NVIC_ISPR.read_volatile() }
    }

    unsafe fn enabled() -> u32 {
        const NVIC_ISER: *const u32 = 0xE000_E100 as *const u32;
        unsafe { NVIC_ISER.read_volatile() }
    }

    unsafe fn set_enabled(mask: u32) {
        const NVIC_ISER: *mut u32 = 0xE000_E100 as *mut u32;
        const NVIC_ICER: *mut u32 = 0xE000_E180 as *mut u32;
        unsafe {
            NVIC_ICER.write_volatile(u32::MAX);
            NVIC_ISER.write_volatile(mask);
        }
    }
}

#[cfg(not(target_os = "none"))]
pub(crate) struct Selected;

#[cfg(not(target_os = "none"))]
impl InterruptController for Selected {
    unsafe fn route(_source: u8, _cpu_int: u8) -> bool {
        true
    }

    fn usable(cpu_int: u8) -> bool {
        // No crossbar off-target: every input is a candidate, so the allocator
        // bookkeeping (first-free, none-free) is exercisable by the host suite.
        cpu_int < 32
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

#[cfg(all(test, feature = "soc-rp2040"))]
mod rp2040_tests {
    use super::rp2040_can_route;

    #[test]
    fn nvic_routes_a_peripheral_to_its_fixed_vector_only() {
        assert!(rp2040_can_route(
            soc_rp2040::IRQ_UART0,
            soc_rp2040::IRQ_UART0
        ));
        assert!(!rp2040_can_route(
            soc_rp2040::IRQ_UART0,
            soc_rp2040::IRQ_SPI0
        ));
        assert!(!rp2040_can_route(
            soc_rp2040::NVIC_IRQ_COUNT,
            soc_rp2040::NVIC_IRQ_COUNT
        ));
    }
}
