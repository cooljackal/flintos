// SPDX-License-Identifier: Apache-2.0

//! Kernel dispatch ABI (plan W5.1, Option A).
//!
//! FlintOS is a single protection domain: `api` calls these `#[no_mangle]`
//! functions directly via `extern "Rust"` linkage — there is no `syscall`
//! instruction. Each function that mutates scheduler/IPC state does so inside a
//! critical section (`scheduler::with` / `cs_with`) so it cannot race the trap
//! handler (plan W2.2).

use crate::debug;
use crate::queue as kqueue;
use crate::scheduler;
use crate::spawn;
use crate::timer;
use hal::types::{Priority, TaskId};

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

/// Spawn pinned to `core`.
///
/// `core` is a raw index rather than a `CoreId` so the syscall boundary stays
/// a plain ABI. Out of range is rejected rather than clamped: a task pinned to
/// a core that does not exist would never be scheduled anywhere, and silently
/// running it on core 0 instead would defeat the reason it was pinned.
#[no_mangle]
pub fn _flint_sys_spawn_on(
    core: u8,
    name: &'static str,
    entry: fn(),
    priority: Priority,
    stack_size: usize,
) -> Option<TaskId> {
    // Refused, not clamped, and refused for a second reason as well: a core
    // that exists but does not run the scheduler would hold the task forever.
    // See `smp::is_pinnable`.
    if !crate::smp::is_pinnable(core) {
        return None;
    }
    spawn::sys_spawn_with_affinity(
        name,
        entry,
        priority,
        stack_size,
        scheduler::Affinity::Core(hal::smp::CoreId(core)),
    )
}

#[no_mangle]
pub fn _flint_sys_yield() {
    if scheduler::with(|sched| sched.yield_current()) {
        scheduler::request_switch();
    }
}

#[no_mangle]
pub fn _flint_sys_sleep_ms(ms: u32) {
    timer::sleep_ms(ms);
}

#[no_mangle]
pub fn _flint_sys_current_id() -> TaskId {
    TaskId(scheduler::with(|s| s.current()))
}

/// Enter the kernel's critical section; returns the state `_flint_sys_cs_exit`
/// needs to leave it. Backs `api::sync::CsCell`, so `api` stays ignorant of
/// which architecture's interrupt mask this is.
///
/// Raw enter/exit rather than a closure-taking `with`, because a closure
/// cannot cross an `extern "Rust"` boundary generically.
#[no_mangle]
pub fn _flint_sys_cs_enter() -> u32 {
    // SAFETY: the caller (`api::sync::CriticalSection`) pairs every enter
    // with exactly one exit, in a `Drop` so a panicking closure still
    // balances.
    unsafe { crate::arch::cs_enter() }
}

/// Leave a critical section entered with `_flint_sys_cs_enter`.
#[no_mangle]
pub fn _flint_sys_cs_exit(saved: u32) {
    // SAFETY: `saved` came from the matching `_flint_sys_cs_enter`; see above.
    unsafe { crate::arch::cs_exit(saved) }
}

#[no_mangle]
pub fn _flint_sys_current_name() -> &'static str {
    scheduler::with(|s| {
        let cur = s.current();
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
pub fn _flint_sys_log_write(level: api::debug::log::Level, args: &core::fmt::Arguments<'_>) {
    debug::log::write(level, args);
}

#[no_mangle]
pub fn _flint_sys_panic(args: &core::fmt::Arguments<'_>) -> ! {
    debug::panic::handle(args)
}
