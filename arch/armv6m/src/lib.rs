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

pub use critical_section::{enter_raw as cs_enter, exit_raw as cs_exit, with as cs_with};

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

/// Default context-selection hook used until the scheduler installs its ARM
/// trap bridge. PendSV still proves the complete register save/restore path.
#[cfg(target_arch = "arm")]
#[no_mangle]
extern "C" fn _flint_armv6m_switch(stack_pointer: u32) -> u32 {
    stack_pointer
}

#[cfg(target_arch = "arm")]
#[no_mangle]
extern "C" fn _flint_armv6m_systick() {
    use hal::tick::TickSource;
    tick::Armv6mTick::tick();
    Armv6mArch::request_switch();
}

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
                lr: task_returned as *const () as usize as u32,
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
            unsafe { ICSR.write_volatile(PENDSVSET) };
            core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
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

extern "C" fn task_returned() -> ! {
    loop {
        Armv6mArch::wait_masked();
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

const _: () = assert!(core::mem::size_of::<TaskContext>() == 4);

#[cfg(test)]
mod tests {
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
}
