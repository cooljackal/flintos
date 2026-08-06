// SPDX-License-Identifier: Apache-2.0

//! Kernel dispatch ABI (plan W5.1, Option A).
//!
//! Flint is a single protection domain: `api` calls these `#[no_mangle]`
//! functions directly via `extern "Rust"` linkage — there is no `syscall`
//! instruction. Each function that mutates scheduler/IPC state does so inside a
//! critical section (`scheduler::with` / `cs_with`) so it cannot race the trap
//! handler (plan W2.2).

use hal::types::{Priority, TaskId};
use crate::scheduler::{self, TaskState};
use crate::spawn;
use crate::timer;
use crate::queue as kqueue;
use crate::debug;

// ── Task syscalls ─────────────────────────────────────────────────────────

#[no_mangle]
pub fn _flint_sys_spawn(
    name: &'static str,
    entry: fn(),
    priority: Priority,
    stack_size: usize,
) -> Option<TaskId> {
    spawn::sys_spawn(name, entry, priority, stack_size)
}

#[no_mangle]
pub fn _flint_sys_yield() {
    scheduler::with(|sched| {
        let cur = sched.current;
        if let Some(tcb) = &mut sched.tasks[cur as usize] {
            tcb.state = TaskState::Ready;
            let prio = tcb.priority;
            sched.ready_mask |= 1u64 << prio;
        }
    });
    scheduler::request_switch();
}

#[no_mangle]
pub fn _flint_sys_sleep_ms(ms: u32) {
    timer::sleep_ms(ms);
}

#[no_mangle]
pub fn _flint_sys_current_id() -> TaskId {
    TaskId(scheduler::with(|s| s.current))
}

#[no_mangle]
pub fn _flint_sys_current_name() -> &'static str {
    scheduler::with(|s| {
        let cur = s.current;
        s.tasks[cur as usize].as_ref().map_or("", |t| t.name)
    })
}

// ── Timer syscalls ─────────────────────────────────────────────────────────

#[no_mangle]
pub fn _flint_sys_timer_now_ms() -> u64 {
    scheduler::with(|s| s.ticks())
}

#[no_mangle]
pub fn _flint_sys_timer_once(ms: u32, cb: fn()) -> u32 {
    timer::once(ms, cb)
}

#[no_mangle]
pub fn _flint_sys_timer_every(ms: u32, cb: fn()) -> u32 {
    timer::every(ms, cb)
}

#[no_mangle]
pub fn _flint_sys_timer_cancel(id: u32) {
    timer::cancel(id);
}

// ── Queue syscalls (plan W4) ───────────────────────────────────────────────

/// Block the calling task waiting to send on a full queue, with timeout.
/// Returns true if a slot became available (caller should retry try_send),
/// false on timeout.
#[no_mangle]
pub fn _flint_sys_queue_block_send(q_addr: usize, timeout_ms: u32) -> bool {
    kqueue::block_send(q_addr, timeout_ms)
}

/// Block the calling task waiting to receive on an empty queue, with timeout.
#[no_mangle]
pub fn _flint_sys_queue_block_recv(q_addr: usize, timeout_ms: u32) -> bool {
    kqueue::block_recv(q_addr, timeout_ms)
}

/// Wake one receiver after a successful send.
#[no_mangle]
pub fn _flint_sys_queue_wake_receiver(q_addr: usize) {
    kqueue::wake_one_receiver(q_addr);
}

/// Wake one sender after a successful receive.
#[no_mangle]
pub fn _flint_sys_queue_wake_sender(q_addr: usize) {
    kqueue::wake_one_sender(q_addr);
}

// ── Mutex syscalls ─────────────────────────────────────────────────────────

#[no_mangle]
pub fn _flint_sys_mutex_lock(mutex: *const core::ffi::c_void) -> bool {
    crate::mutex::lock(mutex as usize)
}

#[no_mangle]
pub fn _flint_sys_mutex_unlock(mutex: *const core::ffi::c_void) {
    crate::mutex::unlock(mutex as usize);
}

// ── Log / panic syscalls ────────────────────────────────────────────────────

#[no_mangle]
pub fn _flint_sys_log_write(
    level: api::debug::log::Level,
    args: &core::fmt::Arguments<'_>,
) {
    debug::log::write(level, args);
}

#[no_mangle]
pub fn _flint_sys_panic(args: &core::fmt::Arguments<'_>) -> ! {
    debug::panic::handle(args)
}
