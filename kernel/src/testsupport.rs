// SPDX-License-Identifier: Apache-2.0

//! Fixtures for the host tests. Compiled only under `cfg(test)`.
//!
//! The scheduler and the mutex table are `static mut` singletons — one set of
//! state for the whole process. That is correct on the target, where there is
//! one kernel, but it means the test harness's default thread-per-test would
//! have several tests mutating the same scheduler at once. The failures would
//! be order-dependent and blamed on the code under test.
//!
//! So every test that touches kernel globals takes [`lock`] first. It resets
//! the world on acquisition, so a test never inherits the previous one's tasks,
//! and holds until the test ends.
//!
//! `--test-threads=1` would also work, but it is a property of how someone
//! happens to invoke cargo. This holds regardless.

use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::scheduler::{self, Scheduler, TaskState};

static KERNEL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Exclusive access to the kernel globals, with everything reset.
///
/// Hold the guard for the whole test:
///
/// ```ignore
/// let _k = testsupport::lock();
/// ```
///
/// The poison is deliberately ignored. A panicking test poisons the mutex, and
/// propagating that would turn one real failure into a cascade of unrelated
/// ones reported as `PoisonError` — hiding the test that actually broke.
pub fn lock() -> MutexGuard<'static, ()> {
    let guard = KERNEL_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reset();
    guard
}

/// Return every kernel global to its power-on state.
///
/// Through the lock like everything else. These helpers used to reach the
/// scheduler directly, which was fine while it was a `static mut` and is not
/// now — and a test harness that bypasses the locking is a harness that cannot
/// catch a locking bug.
fn reset() {
    scheduler::with(|sched| *sched = Scheduler::new());
    crate::mutex::reset_for_test();
    // Every core's, not just this thread's: the flags are per-core now, and a
    // leftover on another core makes the next test's first
    // `take_pending_switch` lie.
    scheduler::clear_all_pending_switches();
}

/// Create a Ready task at `prio` and return its id.
///
/// Enough of a TCB for the scheduler and the mutex code to work with: they read
/// priority, state and the inheritance fields, and nothing else.
pub fn task(prio: u8) -> u32 {
    scheduler::with(|sched| {
        let id = sched.alloc_id().expect("test wanted more tasks than MAX_TASKS");
        if let Some(tcb) = &mut sched.tasks[id as usize] {
            tcb.name = "test";
            tcb.base_prio = prio;
            tcb.priority = prio;
            tcb.state = TaskState::Ready;
            tcb.quantum = scheduler::DEFAULT_QUANTUM_MS;
        }
        sched.ready_mask |= 1u64 << prio;
        id
    })
}

/// Make `id` the running task, as the trap handler would after a switch.
pub fn run(id: u32) {
    scheduler::with(|sched| sched.set_current(id));
}

/// Effective (possibly inherited) priority of `id`.
pub fn prio_of(id: u32) -> u8 {
    scheduler::with(|sched| {
        sched.tasks[id as usize]
            .as_ref()
            .expect("no such task")
            .priority
    })
}

/// Base priority of `id`, ignoring any boost.
pub fn base_of(id: u32) -> u8 {
    scheduler::with(|sched| sched.base_priority(id))
}

pub fn state_of(id: u32) -> TaskState {
    scheduler::with(|sched| {
        sched.tasks[id as usize]
            .as_ref()
            .expect("no such task")
            .state
    })
}

/// Check the invariant the ready mask exists to maintain: a bit is set exactly
/// when some task at that priority is runnable.
///
/// Every scheduler operation must preserve this. It is worth asserting after
/// each one rather than only at the end, because the two ways to break it fail
/// very differently — a bit set with nothing behind it makes `schedule()` pick
/// a task that is not runnable, while a bit clear with a Ready task behind it
/// strands that task forever with its TCB still claiming it is Ready. The
/// second is silent, and is exactly the bug that shipped in `flint_task_exit`.
#[track_caller]
pub fn assert_ready_mask_consistent() {
    scheduler::with(|sched| {
    for prio in 0..scheduler::NUM_PRIORITIES as u8 {
        let runnable = sched.tasks.iter().flatten().any(|t| {
            t.priority == prio && matches!(t.state, TaskState::Ready | TaskState::Running)
        });
        let bit = sched.ready_mask & (1u64 << prio) != 0;
        assert_eq!(
            bit,
            runnable,
            "ready_mask bit {prio} is {}, but {} runnable task(s) sit at that priority",
            if bit { "set" } else { "clear" },
            sched
                .tasks
                .iter()
                .flatten()
                .filter(|t| t.priority == prio
                    && matches!(t.state, TaskState::Ready | TaskState::Running))
                .count()
        );
    }
    });
}
