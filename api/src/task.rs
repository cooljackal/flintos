// SPDX-License-Identifier: Apache-2.0

//! Tasks: start them, sleep, yield, wait for a condition, and leave.
//!
//! ```ignore
//! use api::task::{self, Task};
//!
//! let Some(id) = Task::new("sensor", sensor).priority(Priority::Normal(2)).spawn() else {
//!     log_error!("task pool full");
//!     task::exit();
//! };
//! ```

use hal::types::{Priority, TaskId};

/// Priority a [`Task`] gets when none is asked for. The most common choice
/// in the tree's applications.
pub const DEFAULT_PRIORITY: Priority = Priority::Normal(1);
/// Stack size a [`Task`] gets when none is asked for, in bytes. Enough for an
/// application task that logs and talks to a driver; radio work wants more.
pub const DEFAULT_STACK: usize = 4096;

/// Spawn a new task.
pub fn spawn(
    name: &'static str,
    entry: fn(),
    priority: Priority,
    stack_size: usize,
) -> Option<TaskId> {
    // Implemented in the kernel crate — this is the syscall interface.
    extern "Rust" {
        fn _flint_sys_spawn(
            name: &'static str,
            entry: fn(),
            priority: Priority,
            stack_size: usize,
        ) -> Option<TaskId>;
    }
    unsafe { _flint_sys_spawn(name, entry, priority, stack_size) }
}

/// Spawn a task pinned to one core.
///
/// [`spawn`] means "either core". This means "this core, always" — for a task
/// that cannot float: a driver whose peripheral interrupt is routed to one
/// core's matrix, or work whose timing budget a migration would blow.
///
/// Returns `None` if `core` is not a core this build runs on. It is not
/// clamped to core 0: a task that asked to be pinned and silently was not is
/// worse than one that failed to start.
pub fn spawn_on(
    core: u8,
    name: &'static str,
    entry: fn(),
    priority: Priority,
    stack_size: usize,
) -> Option<TaskId> {
    extern "Rust" {
        fn _flint_sys_spawn_on(
            core: u8,
            name: &'static str,
            entry: fn(),
            priority: Priority,
            stack_size: usize,
        ) -> Option<TaskId>;
    }
    unsafe { _flint_sys_spawn_on(core, name, entry, priority, stack_size) }
}

/// Describe a task before starting it.
///
/// Only the name and the entry point are required; priority, stack size and
/// core pinning fall back to [`DEFAULT_PRIORITY`], [`DEFAULT_STACK`] and
/// "either core". Nothing runs until [`spawn`](Self::spawn) is called, which
/// is why the builder is `#[must_use]`.
#[must_use = "a Task does nothing until .spawn() is called"]
#[derive(Debug, Clone, Copy)]
pub struct Task {
    name: &'static str,
    entry: fn(),
    priority: Priority,
    stack_size: usize,
    core: Option<u8>,
}

impl Task {
    /// A task with default priority and stack, free to run on any core.
    pub const fn new(name: &'static str, entry: fn()) -> Self {
        Self {
            name,
            entry,
            priority: DEFAULT_PRIORITY,
            stack_size: DEFAULT_STACK,
            core: None,
        }
    }

    /// Run at `priority` instead of [`DEFAULT_PRIORITY`].
    pub const fn priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Give the task `bytes` of stack instead of [`DEFAULT_STACK`].
    pub const fn stack(mut self, bytes: usize) -> Self {
        self.stack_size = bytes;
        self
    }

    /// Pin the task to `core`. See [`spawn_on`] for when that is the right
    /// call and what happens if the core does not exist.
    pub const fn on_core(mut self, core: u8) -> Self {
        self.core = Some(core);
        self
    }

    /// Start the task.
    ///
    /// `None` means it did not start: the task pool is full, or the core it
    /// was pinned to is not one this build runs on. The result is
    /// `#[must_use]` because a task that silently failed to start is the
    /// kind of bug that only shows up as "nothing happens".
    #[must_use = "None means the task did not start; handle it"]
    pub fn spawn(self) -> Option<TaskId> {
        match self.core {
            Some(core) => spawn_on(core, self.name, self.entry, self.priority, self.stack_size),
            None => spawn(self.name, self.entry, self.priority, self.stack_size),
        }
    }
}

/// End the calling task. It never runs again.
///
/// The way out of a task's entry function when there is nothing left to do
/// — a failed bring-up, a one-shot job that finished — instead of parking
/// in a `loop { sleep_ms(..) }` that keeps waking up for nothing.
pub fn exit() -> ! {
    extern "Rust" {
        fn _flint_sys_task_exit() -> !;
    }
    unsafe { _flint_sys_task_exit() }
}

/// Poll `cond` until it is true or `timeout_ms` has passed.
///
/// Returns `true` if the condition was met, `false` on timeout. Sleeps one
/// millisecond between polls, so other tasks run meanwhile; with a
/// `timeout_ms` of zero the condition is checked exactly once.
///
/// Measures elapsed time with [`timer::now_ms`](crate::timer::now_ms), so it
/// is only as fine-grained as the kernel tick.
pub fn wait_until(mut cond: impl FnMut() -> bool, timeout_ms: u32) -> bool {
    let start = crate::timer::now_ms();
    loop {
        if cond() {
            return true;
        }
        if crate::timer::now_ms().wrapping_sub(start) >= u64::from(timeout_ms) {
            return false;
        }
        sleep_ms(1);
    }
}

/// Yield the current task's remaining quantum.
pub fn yield_now() {
    extern "Rust" {
        fn _flint_sys_yield();
    }
    unsafe { _flint_sys_yield() }
}

/// Sleep for `ms` milliseconds.  Blocks the current task.
pub fn sleep_ms(ms: u32) {
    extern "Rust" {
        fn _flint_sys_sleep_ms(ms: u32);
    }
    unsafe { _flint_sys_sleep_ms(ms) }
}

/// Return the ID of the currently executing task.
pub fn current_id() -> TaskId {
    extern "Rust" {
        fn _flint_sys_current_id() -> TaskId;
    }
    unsafe { _flint_sys_current_id() }
}

/// Return the name of the currently executing task.
pub fn current_name() -> &'static str {
    extern "Rust" {
        fn _flint_sys_current_name() -> &'static str;
    }
    unsafe { _flint_sys_current_name() }
}

// ── Tests ──────────────────────────────────────────────────────────────────
//
// Only the builder's bookkeeping is host-testable: `spawn`, `exit` and
// `wait_until` bottom out in kernel syscalls that exist on a target.

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    fn entry() {}

    #[test]
    fn builder_defaults() {
        let t = Task::new("t", entry);
        assert_eq!(t.name, "t");
        assert_eq!(t.priority, DEFAULT_PRIORITY);
        assert_eq!(t.stack_size, DEFAULT_STACK);
        assert_eq!(t.core, None);
    }

    #[test]
    fn builder_overrides() {
        let t = Task::new("t", entry)
            .priority(Priority::Critical(3))
            .stack(16384)
            .on_core(1);
        assert_eq!(t.priority, Priority::Critical(3));
        assert_eq!(t.stack_size, 16384);
        assert_eq!(t.core, Some(1));
    }

    #[test]
    fn builder_is_const() {
        const T: Task = Task::new("c", entry).stack(2048);
        assert_eq!(T.stack_size, 2048);
    }
}
