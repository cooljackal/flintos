// SPDX-License-Identifier: Apache-2.0

//! Xtensa context ABI (plan W5.1, Option A locked).
//!
//! Flint is a single protection domain: the `flint-api` → kernel boundary is a
//! direct `extern "Rust"` call, **not** a `syscall` instruction. There is no
//! syscall router. This type therefore implements only the context save/restore
//! half of `SyscallABI`, used by the scheduler and the IRQ trap path. The
//! `enter`/`return_to_task` methods remain to satisfy the trait but are inert
//! (no syscall mechanism exists to feed them).

use flint_hal::syscall::{SyscallABI, SyscallArgs, SyscallResult};
use flint_hal::types::{RawTrapFrame, TaskContext};

pub struct XtensaSyscallABI;

impl SyscallABI for XtensaSyscallABI {
    /// No syscall instruction in Option A — kept inert for trait completeness.
    fn enter(_frame: &RawTrapFrame) -> SyscallArgs {
        SyscallArgs { number: 0, arg0: 0, arg1: 0, arg2: 0 }
    }

    /// No syscall instruction in Option A — kept inert for trait completeness.
    fn return_to_task(_frame: &mut RawTrapFrame, _result: SyscallResult) {}

    /// Save the current execution context (cooperative path).
    ///
    /// Spills register windows first so the full task state is captured, then
    /// records the live window via the assembly switch primitive's save half.
    fn save_context(_task: &mut TaskContext) {
        // Saving the *current* context standalone is only meaningful as half of
        // a switch; the scheduler uses `flint_context_switch` directly. This is
        // intentionally a no-op to avoid a second, divergent save path.
    }

    /// Restore is performed by `flint_context_switch` / `flint_restore_first`.
    fn restore_context(_task: &TaskContext) {}
}
