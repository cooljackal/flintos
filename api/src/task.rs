// SPDX-License-Identifier: Apache-2.0

use hal::types::{TaskId, Priority};

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
