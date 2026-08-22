// SPDX-License-Identifier: Apache-2.0

//! Contract between portable kernel state and an architecture's saved frame.

/// A saved task context that can initialize static scheduler storage.
pub trait TaskContext: Sized {
    /// Empty context used before a task is spawned.
    const ZERO: Self;
}

/// Architecture-neutral interrupt sources observed at one trap entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptCause {
    pub tick: bool,
    pub switch_request: bool,
    /// External interrupt numbers, one bit per kernel IRQ slot.
    pub external: u32,
}

/// Register values needed by the kernel's fatal-fault report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultInfo {
    pub cause: u32,
    pub pc: u32,
    pub status: u32,
    pub address: u32,
    pub arg0: u32,
    pub arg1: u32,
}

/// Meaning of the exception that entered the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapCause {
    Interrupt(InterruptCause),
    Fault(FaultInfo),
}

/// Small, optional context view used only by trap diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextDiagnostics {
    pub pc: u32,
    pub architecture_state: u32,
}

/// Architecture operations that own the shape and initialization of a task.
pub trait Architecture {
    /// Register frame saved and restored by this architecture's trap entry.
    type Context: TaskContext;

    /// Build the first frame for `entry` at the top of a fresh task stack.
    ///
    /// # Safety
    /// `stack_top` must name writable memory with the architecture's required
    /// stack geometry and alignment.
    unsafe fn init_context(context: &mut Self::Context, entry: usize, stack_top: u32);

    /// Save the frame built by trap entry into a task's persistent context.
    ///
    /// # Safety
    /// `frame` must point to one valid context produced by this architecture.
    unsafe fn save_context(frame: *const Self::Context, saved: &mut Self::Context);

    /// Return the frame pointer the architecture's trap exit must restore.
    fn restore_context(saved: &mut Self::Context) -> *mut Self::Context;

    /// Raise the architecture's deferred context-switch exception.
    fn request_switch();

    /// Park until an interrupt is eligible to run.
    fn wait_for_interrupt();

    /// Park with maskable interrupts disabled; used only by terminal halts.
    fn wait_masked();

    /// Read a wrapping free-running cycle count, when the target has one.
    fn cycle_count() -> Option<u32>;

    /// Decode the current exception without exposing processor registers.
    ///
    /// # Safety
    /// Must be called from this architecture's trap handler with `frame`
    /// pointing to the frame produced by trap entry.
    unsafe fn trap_cause(frame: *const Self::Context) -> TrapCause;

    /// Acknowledge the deferred-switch source after it has been decoded.
    fn acknowledge_switch_request();

    /// Values useful for optional context-switch diagnostics.
    fn context_diagnostics(context: &Self::Context) -> ContextDiagnostics;
}
