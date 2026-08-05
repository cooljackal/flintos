//! Architecture-specific syscall ABI.
//!
//! Each architecture (Xtensa, ARM) provides an `impl SyscallABI` that
//! handles the calling convention, register windowing, and privilege
//! level differences between kernel and user mode.

use crate::types::*;

/// Architecture-specific syscall ABI.
///
/// Methods are called from the arch-specific exception entry stubs and
/// from the scheduler during context switches.
pub trait SyscallABI {
    /// Extract syscall arguments from the raw trap frame.
    ///
    /// Called from the arch exception entry immediately after the
    /// `_rust_exception_handler` receives the frame pointer.
    fn enter(frame: &RawTrapFrame) -> SyscallArgs;

    /// Prepare the trap frame so that returning from the exception
    /// delivers the result to the caller in the correct register.
    fn return_to_task(frame: &mut RawTrapFrame, result: SyscallResult);

    /// Save the current execution context into a `TaskContext` for
    /// later restoration by the scheduler.
    fn save_context(task: &mut TaskContext);

    /// Restore an execution context previously saved by `save_context`.
    fn restore_context(task: &TaskContext);
}

/// Syscall number and up to three word-sized arguments.
pub struct SyscallArgs {
    pub number: u32,
    pub arg0: u32,
    pub arg1: u32,
    pub arg2: u32,
}

/// Return value from a syscall.
pub struct SyscallResult {
    pub value: u32,
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn syscall_args_construct() {
        let args = SyscallArgs { number: 1, arg0: 10, arg1: 20, arg2: 30 };
        assert_eq!(args.number, 1);
        assert_eq!(args.arg0, 10);
    }

    #[test]
    fn syscall_result_construct() {
        let result = SyscallResult { value: 42 };
        assert_eq!(result.value, 42);
    }
}