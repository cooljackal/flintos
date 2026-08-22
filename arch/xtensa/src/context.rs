// SPDX-License-Identifier: Apache-2.0

//! Xtensa register frame and first-dispatch construction.

use crate::registers::PS_WOE;

/// Saved execution state built and restored by `vectors.S`.
///
/// The assembly trap entry addresses every field by a fixed offset. This type,
/// the assembly frame, the scheduler's stored context, and `_flint_trap` must
/// therefore remain one layout.
#[repr(C)]
pub struct TaskContext {
    pub pc: u32,
    pub ps: u32,
    pub sar: u32,
    pub lbeg: u32,
    pub lend: u32,
    pub lcount: u32,
    pub a: [u32; 16],
    pub windowbase: u32,
    pub windowstart: u32,
    /// Comparand used by `S32C1I`; trap handlers must not clobber it.
    ///
    /// LLVM lowers an atomic read-modify-write to a retry loop that writes this
    /// register. Saving it prevents an interrupt handler's atomic operation
    /// from corrupting the interrupted loop. NuttX `xtensa_context.S` and
    /// Zephyr's Xtensa saved frame both preserve it for the same reason.
    pub scompare1: u32,
    pub _reserved: [u32; 3],
}

impl TaskContext {
    pub const ZERO: Self = Self {
        pc: 0,
        ps: 0,
        sar: 0,
        lbeg: 0,
        lend: 0,
        lcount: 0,
        a: [0; 16],
        windowbase: 0,
        windowstart: 0,
        scompare1: 0,
        _reserved: [0; 3],
    };
}

impl hal::arch::TaskContext for TaskContext {
    const ZERO: Self = Self::ZERO;
}

pub struct XtensaArch;

impl hal::arch::Architecture for XtensaArch {
    type Context = TaskContext;

    unsafe fn init_context(context: &mut TaskContext, entry: usize, stack_top: u32) {
        extern "C" {
            fn _flint_task_start();
        }

        const BASE_SAVE_AREA: u32 = 32;
        // The trampoline uses a real callx4 so hardware establishes CALLINC,
        // the return address, and the overlapping register-window state. A
        // hand-built direct entry frame does not match any hardware call.
        context.pc = _flint_task_start as *const () as usize as u32;
        context.ps = PS_WOE;
        context.sar = 0;
        context.lbeg = 0;
        context.lend = 0;
        context.lcount = 0;

        let sp = (stack_top - BASE_SAVE_AREA) & !15;
        context.a = [0; 16];
        context.a[0] = 0;
        context.a[1] = sp;
        context.a[3] = entry as u32;
        context.a[5] = sp;
        context.a[9] = sp;
        context.a[13] = sp;
        context.windowbase = 0;
        context.windowstart = 1;
    }

    unsafe fn save_context(frame: *const TaskContext, saved: &mut TaskContext) {
        unsafe { core::ptr::copy_nonoverlapping(frame, saved, 1) };
    }

    fn restore_context(saved: &mut TaskContext) -> *mut TaskContext {
        saved
    }

    fn request_switch() {
        unsafe { crate::registers::request_switch() };
    }

    fn wait_for_interrupt() {
        unsafe { core::arch::asm!("waiti 0") };
    }

    fn wait_masked() {
        unsafe { core::arch::asm!("waiti 15") };
    }

    fn cycle_count() -> Option<u32> {
        Some(unsafe { crate::registers::read_ccount() })
    }

    unsafe fn trap_cause(frame: *const TaskContext) -> hal::arch::TrapCause {
        use hal::arch::{FaultInfo, InterruptCause, TrapCause};

        let cause = unsafe { crate::registers::read_exccause() };
        if cause == crate::registers::EXCCAUSE_LEVEL1_INTERRUPT {
            let pending =
                unsafe { crate::registers::read_interrupt() & crate::registers::read_intenable() };
            TrapCause::Interrupt(InterruptCause {
                tick: pending & crate::registers::INT_TIMER0_MASK != 0,
                switch_request: pending & crate::registers::INT_SOFTWARE_MASK != 0,
                external: pending
                    & !(crate::registers::INT_TIMER0_MASK | crate::registers::INT_SOFTWARE_MASK),
            })
        } else {
            let context = unsafe { &*frame };
            TrapCause::Fault(FaultInfo {
                cause,
                pc: context.pc,
                status: context.ps,
                address: unsafe { crate::registers::read_excvaddr() },
                arg0: context.a[0],
                arg1: context.a[1],
            })
        }
    }

    fn acknowledge_switch_request() {
        unsafe { crate::registers::intclear(crate::registers::INT_SOFTWARE_MASK) };
    }

    fn context_diagnostics(context: &TaskContext) -> hal::arch::ContextDiagnostics {
        hal::arch::ContextDiagnostics {
            pc: context.pc,
            architecture_state: context.windowstart,
        }
    }
}

const _: () = {
    assert!(core::mem::size_of::<TaskContext>() == 112);
    assert!(core::mem::size_of::<TaskContext>() % 16 == 0);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_context_matches_reset_state() {
        let context = TaskContext::ZERO;
        assert_eq!(context.pc, 0);
        assert_eq!(context.ps, 0);
        assert_eq!(context.a, [0; 16]);
        assert_eq!(context.windowbase, 0);
        assert_eq!(context.windowstart, 0);
    }
}
