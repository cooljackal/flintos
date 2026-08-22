// SPDX-License-Identifier: Apache-2.0

//! ARMv6-M primitives shared by Cortex-M0+ targets.
//!
//! RP2040-specific clock, reset, boot2, and peripheral setup do not belong
//! here. The one chip-specific mechanism is the critical-section backend:
//! RP2040 needs its SIO spinlock because PRIMASK excludes only the local core.

#![no_std]

#[cfg(target_arch = "arm")]
core::arch::global_asm!(include_str!("startup.S"), options(raw));

mod critical_section;
pub mod smp;
pub mod tick;

pub use critical_section::{
    enter_raw as cs_enter, exit_raw as cs_exit, init_boot_core, with as cs_with,
};

use hal::arch::{
    Architecture, ContextDiagnostics, FaultInfo, InterruptCause, TaskContext as TaskContextTrait,
    TrapCause,
};

/// Pointer to the software-plus-hardware frame on a task's own stack.
#[repr(C)]
pub struct TaskContext {
    pub stack_pointer: u32,
}

impl TaskContextTrait for TaskContext {
    const ZERO: Self = Self { stack_pointer: 0 };
}

/// Registers fabricated on a new task's stack.
///
/// PendSV saves r4-r11 first; exception return then consumes the eight-word
/// hardware frame beginning at r0.
#[repr(C)]
struct InitialFrame {
    r4_r11: [u32; 8],
    r0: u32,
    r1: u32,
    r2: u32,
    r3: u32,
    r12: u32,
    lr: u32,
    pc: u32,
    xpsr: u32,
}

pub struct Armv6mArch;

fn initial_frame_address(stack_top: u32) -> u32 {
    (stack_top - core::mem::size_of::<InitialFrame>() as u32) & !7
}

fn decode_exception(number: u32) -> TrapCause {
    match number {
        14 => TrapCause::Interrupt(InterruptCause {
            tick: false,
            switch_request: true,
            external: 0,
        }),
        15 => TrapCause::Interrupt(InterruptCause {
            tick: true,
            switch_request: false,
            external: 0,
        }),
        16..=47 => TrapCause::Interrupt(InterruptCause {
            tick: false,
            switch_request: false,
            external: 1 << (number - 16),
        }),
        _ => TrapCause::Fault(FaultInfo {
            cause: number,
            pc: 0,
            status: 0,
            address: 0,
            arg0: 0,
            arg1: 0,
        }),
    }
}

impl Architecture for Armv6mArch {
    type Context = TaskContext;

    unsafe fn init_context(context: &mut TaskContext, entry: usize, stack_top: u32) {
        unsafe extern "C" {
            fn _flint_armv6m_task_exit() -> !;
        }
        const THUMB_BIT: u32 = 1 << 24;
        let frame_address = initial_frame_address(stack_top);
        let frame = frame_address as *mut InitialFrame;
        unsafe {
            frame.write(InitialFrame {
                r4_r11: [0; 8],
                r0: 0,
                r1: 0,
                r2: 0,
                r3: 0,
                r12: 0,
                lr: _flint_armv6m_task_exit as *const () as usize as u32,
                pc: entry as u32,
                xpsr: THUMB_BIT,
            });
        }
        context.stack_pointer = frame_address;
    }

    unsafe fn save_context(frame: *const TaskContext, saved: &mut TaskContext) {
        saved.stack_pointer = unsafe { (*frame).stack_pointer };
    }

    fn restore_context(saved: &mut TaskContext) -> *mut TaskContext {
        saved
    }

    fn request_switch() {
        #[cfg(target_arch = "arm")]
        {
            const ICSR: *mut u32 = 0xe000_ed04 as *mut u32;
            const PENDSVSET: u32 = 1 << 28;
            unsafe {
                ICSR.write_volatile(PENDSVSET);
                core::arch::asm!("dsb", "isb", options(nostack));
            }
        }
    }

    fn wait_for_interrupt() {
        #[cfg(target_arch = "arm")]
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack))
        };
        #[cfg(not(target_arch = "arm"))]
        panic!("wait_for_interrupt is target-only");
    }

    fn wait_masked() {
        #[cfg(target_arch = "arm")]
        unsafe {
            core::arch::asm!("cpsid i", "wfi", options(nomem, nostack))
        };
        #[cfg(not(target_arch = "arm"))]
        panic!("wait_masked is target-only");
    }

    fn mask_interrupts() -> u32 {
        #[cfg(target_arch = "arm")]
        {
            let primask: u32;
            unsafe {
                core::arch::asm!(
                    "mrs {state}, PRIMASK",
                    "cpsid i",
                    state = out(reg) primask,
                    options(nomem, nostack)
                );
            }
            primask
        }
        #[cfg(not(target_arch = "arm"))]
        {
            0
        }
    }

    fn cycle_count() -> Option<u32> {
        None
    }

    unsafe fn trap_cause(_frame: *const TaskContext) -> TrapCause {
        #[cfg(target_arch = "arm")]
        {
            let exception: u32;
            unsafe {
                core::arch::asm!("mrs {number}, IPSR", number = out(reg) exception, options(nomem, nostack));
            }
            decode_exception(exception)
        }
        #[cfg(not(target_arch = "arm"))]
        decode_exception(14)
    }

    fn acknowledge_switch_request() {}

    fn context_diagnostics(context: &TaskContext) -> ContextDiagnostics {
        let hardware = (context.stack_pointer + 8 * 4) as *const u32;
        ContextDiagnostics {
            pc: unsafe { hardware.add(6).read() },
            architecture_state: context.stack_pointer,
        }
    }
}

/// Convert a Cortex-M fault frame into the kernel's neutral diagnostic form.
///
/// # Safety
/// `frame` must point to the eight-word hardware exception frame supplied by
/// the Cortex-M exception entry sequence and remain readable for this call.
pub unsafe fn fault_info(frame: *const u32, cause: u32, address: u32) -> FaultInfo {
    FaultInfo {
        cause,
        pc: unsafe { frame.add(6).read() },
        status: unsafe { frame.add(7).read() },
        address,
        arg0: unsafe { frame.read() },
        arg1: unsafe { frame.add(1).read() },
    }
}

/// Last Cortex-M hardware frame captured by `HardFault`.
#[cfg(target_arch = "arm")]
#[no_mangle]
#[link_section = ".uninit.hard_fault"]
pub static mut FLINT_ARMV6M_HARD_FAULT: FaultInfo = FaultInfo {
    cause: 0,
    pc: 0,
    status: 0,
    address: 0,
    arg0: 0,
    arg1: 0,
};

#[cfg(target_arch = "arm")]
#[no_mangle]
unsafe extern "C" fn _flint_armv6m_hard_fault(frame: *const u32, exc_return: u32) -> ! {
    let captured = unsafe { fault_info(frame, 3, exc_return) };
    unsafe { core::ptr::write_volatile(&raw mut FLINT_ARMV6M_HARD_FAULT, captured) };
    unsafe extern "C" {
        fn _flint_armv6m_fault_observed(pc: u32) -> !;
    }
    unsafe { _flint_armv6m_fault_observed(captured.pc) }
}

const _: () = assert!(core::mem::size_of::<TaskContext>() == 4);

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn initial_frame_matches_software_and_hardware_stack_words() {
        assert_eq!(core::mem::size_of::<InitialFrame>(), 16 * 4);
    }

    #[test]
    fn initial_frame_preserves_exception_stack_alignment() {
        assert_eq!(initial_frame_address(0x2000_1004), 0x2000_0fc0);
        assert_eq!(initial_frame_address(0x2000_1000), 0x2000_0fc0);
    }

    #[test]
    fn host_stand_in_reports_core_zero() {
        use hal::smp::MultiCore;
        assert_eq!(smp::Armv6mSmp::current_core().0, 0);
    }

    #[test]
    fn cortex_exception_numbers_map_to_portable_causes() {
        assert!(matches!(decode_exception(14), TrapCause::Interrupt(c) if c.switch_request));
        assert!(matches!(decode_exception(15), TrapCause::Interrupt(c) if c.tick));
        assert!(matches!(decode_exception(18), TrapCause::Interrupt(c) if c.external == 4));
        assert!(matches!(decode_exception(3), TrapCause::Fault(f) if f.cause == 3));
    }

    #[test]
    fn hard_fault_frame_captures_cortex_stacked_registers() {
        let frame = [11, 22, 0, 0, 0, 0, 0x1000_0123, 1 << 24];
        let fault = unsafe { fault_info(frame.as_ptr(), 3, 0) };
        assert_eq!(fault.cause, 3);
        assert_eq!(fault.pc, 0x1000_0123);
        assert_eq!(fault.status, 1 << 24);
        assert_eq!((fault.arg0, fault.arg1), (11, 22));
    }

    #[test]
    fn restores_psp_after_all_eight_software_saved_words() {
        let startup = include_str!("startup.S");
        assert_eq!(startup.matches("msr psp, r1").count(), 2);
        assert!(!startup.contains("msr psp, r0"));
        assert_eq!(
            startup
                .matches("adds r1, #16\n    ldmia r1!, {r4-r7}")
                .count(),
            2
        );
    }

    #[test]
    fn first_launch_stays_masked_until_psp_and_exc_return_are_ready() {
        let startup = include_str!("startup.S");
        let reset = startup
            .find("Reset:\n    cpsid i")
            .expect("Reset masks interrupts");
        let svc = startup.find("SVC:").expect("SVC handler");
        let enable = startup[svc..]
            .find("cpsie i\n    bx lr")
            .expect("SVC enables only immediately before exception return");
        assert!(reset < svc);
        assert!(startup[reset..svc].contains("cpsie i\n    svc 0"));
        assert!(startup[svc..svc + enable].contains("_flint_armv6m_start_tick"));
        assert!(startup[svc..svc + enable].contains("msr psp, r1"));
    }

    #[test]
    fn cortex_vectors_put_hard_fault_and_svc_in_their_architected_slots() {
        let startup = include_str!("startup.S");
        let table = startup
            .split("_vector_table_start:\n")
            .nth(1)
            .expect("vector table")
            .split(".section .text.Reset")
            .next()
            .expect("vector entries");
        let entries: std::vec::Vec<_> = table
            .lines()
            .filter_map(|line| line.trim().strip_prefix(".word "))
            .collect();
        assert_eq!(entries[2], "DefaultHandler");
        assert_eq!(entries[3], "HardFault");
        assert_eq!(entries[11], "SVC");
        assert_eq!(entries[14], "PendSV");
        assert_eq!(entries[15], "SysTick");
    }
}
