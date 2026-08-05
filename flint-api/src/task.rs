use flint_hal::types::{TaskId, Priority};

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
