// SPDX-License-Identifier: Apache-2.0

//! Preemptive priority scheduler.
//!
//! 48 application priority levels plus a reserved idle level (lower number =
//! higher priority), round-robin within a level via a 10 ms quantum. Supports
//! priority inheritance, sleep, queue/mutex blocking, and a pending-switch flag
//! consumed by the trap handler.
//!
//! Switching model (plan W1.2): **all** context switches happen in the trap
//! handler (`crate::switch::flint_trap`) and resume via `rfe`. Cooperative
//! operations (sleep/yield/block) mutate scheduler state then raise the software
//! interrupt so the switch occurs through the same path. Nothing switches by
//! returning up a call stack anymore.
//!
//! Concurrency (plan W2.2): the global scheduler is mutated either from task
//! context inside a `cs_with` critical section, or from the trap handler with
//! interrupts already masked. Both exclude each other.

use core::sync::atomic::{AtomicBool, Ordering};
use flint_hal::tick::TickSource;
use flint_hal::types::TaskContext;
use flint_arch_xtensa::tick::XtensaTick;

pub const MAX_TASKS: usize = 32;

/// Effective priority levels.
///
/// The public `Priority` encoding spans 0x00..=0x2F (3 bands × 16 levels, see
/// `Priority::numeric`), so 48 values are reachable from application code. One
/// further level exists above them, reserved for idle -- hence 49.
pub const NUM_PRIORITIES: usize = 49;

/// Highest priority value any public `Priority` can encode to
/// (`Background(15)` == 0x2F).
pub const MAX_PUBLIC_PRIORITY: u8 = 0x2F;

pub const DEFAULT_QUANTUM_MS: u32 = 10;

/// The lowest possible priority value (idle). Higher number = lower priority.
///
/// Deliberately one below anything application code can request. When this was
/// 47 it collided with `Background(15)`, so a task spawned at the lowest public
/// priority became a round-robin peer of idle and shared time-slices with it,
/// rather than idle being a guaranteed last resort.
pub const IDLE_PRIORITY: u8 = (NUM_PRIORITIES - 1) as u8;

const _: () = assert!(
    IDLE_PRIORITY > MAX_PUBLIC_PRIORITY,
    "idle must sit below every priority application code can request"
);
const _: () = assert!(
    NUM_PRIORITIES <= 64,
    "ready_mask is a u64; one bit per priority level"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Init,
    Ready,
    Running,
    BlockedSend,
    BlockedRecv,
    BlockedMutex,
    BlockedSleep,
    Suspended,
    /// The task panicked. Terminal: nothing moves a task out of this state.
    ///
    /// A panic halts the whole system (see `debug::panic`), so no scheduling
    /// happens after one is set. The state exists so a TCB read through a
    /// debugger says what actually happened rather than still claiming to be
    /// Running.
    Faulted,
}

/// Per-task control block.
#[repr(C)]
pub struct TaskControlBlock {
    pub id: u32,
    pub name: &'static str,
    pub entry: Option<fn()>,
    /// The task's own (base) priority.
    pub base_prio: u8,
    /// Current effective priority (base, or boosted by inheritance).
    pub priority: u8,
    pub state: TaskState,
    pub stack_base: u32,
    pub stack_size: u32,
    pub stack_hwm: u32,
    pub context: TaskContext,
    pub quantum: u32,
    pub sleep_until: u64,
    /// Set while the task's priority is boosted by inheritance: the value to
    /// fall back to. `None` means "not boosted".
    pub boosted_from: Option<u8>,
    /// Which mutex this task is blocked on (address), if any.
    pub blocked_on_mutex: Option<usize>,
}

impl TaskControlBlock {
    const fn zeroed() -> Self {
        Self {
            id: u32::MAX,
            name: "",
            entry: None,
            base_prio: 0,
            priority: 0,
            state: TaskState::Init,
            stack_base: 0,
            stack_size: 0,
            stack_hwm: 0,
            context: TaskContext::zeroed(),
            quantum: 0,
            sleep_until: 0,
            boosted_from: None,
            blocked_on_mutex: None,
        }
    }
}

pub struct Scheduler {
    pub tasks: [Option<TaskControlBlock>; MAX_TASKS],
    pub current: u32,
    /// One bit per effective priority level with at least one ready task.
    pub ready_mask: u64,
    /// Round-robin rotor: last task index dispatched at each priority level.
    last_run: [u32; NUM_PRIORITIES],
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            tasks: [const { None }; MAX_TASKS],
            current: u32::MAX,
            ready_mask: 0,
            last_run: [0; NUM_PRIORITIES],
        }
    }

    /// Allocate a task slot.
    pub fn alloc_id(&mut self) -> Option<u32> {
        for i in 0..MAX_TASKS {
            if self.tasks[i].is_none() {
                self.tasks[i] = Some(TaskControlBlock {
                    id: i as u32,
                    ..TaskControlBlock::zeroed()
                });
                return Some(i as u32);
            }
        }
        None
    }

    fn set_ready_bit(&mut self, prio: u8) {
        self.ready_mask |= 1u64 << prio;
    }

    /// Rebuild the whole ready mask from the tasks' states.
    ///
    /// The per-priority version below is what the hot paths use. This one is
    /// for the case where a task left the run set in a way that does not fit
    /// the usual transitions -- currently only a panic -- and the cost of
    /// scanning every TCB does not matter because the system is about to stop.
    pub fn recompute_ready_mask(&mut self) {
        self.ready_mask = 0;
        for prio in 0..NUM_PRIORITIES as u8 {
            self.recompute_ready_bit(prio);
        }
    }

    /// Recompute the ready bit for a priority level by scanning for any Ready
    /// task at that level (used after a task leaves Ready or changes priority).
    pub(crate) fn recompute_ready_bit(&mut self, prio: u8) {
        let any = self.tasks.iter().flatten().any(|t| {
            t.priority == prio && matches!(t.state, TaskState::Ready | TaskState::Running)
        });
        if any {
            self.ready_mask |= 1u64 << prio;
        } else {
            self.ready_mask &= !(1u64 << prio);
        }
    }

    pub fn make_ready(&mut self, id: u32) {
        if let Some(tcb) = &mut self.tasks[id as usize] {
            tcb.state = TaskState::Ready;
            tcb.quantum = DEFAULT_QUANTUM_MS;
            let prio = tcb.priority;
            // `tcb` borrows `self.tasks`; `ready_mask` is a disjoint field, so
            // set it inline rather than via `self.set_ready_bit` (which would
            // re-borrow all of `self`).
            self.ready_mask |= 1u64 << prio;
        }
    }

    pub fn set_current(&mut self, id: u32) {
        self.current = id;
        if let Some(tcb) = &mut self.tasks[id as usize] {
            tcb.state = TaskState::Running;
        }
    }

    /// The single authoritative tick count (owned by the tick source).
    pub fn ticks(&self) -> u64 {
        XtensaTick::now()
    }

    pub fn current_priority(&self) -> u8 {
        self.tasks[self.current as usize]
            .as_ref()
            .map_or(IDLE_PRIORITY, |t| t.priority)
    }

    /// Advance scheduling state for one tick. `now` is the current tick count.
    /// Returns true if a context switch should happen.
    pub fn on_tick(&mut self, now: u64) -> bool {
        let mut need_switch = false;
        let cur_prio = self.current_priority();

        // Wake any timed-out blocked tasks. `sleep_until == 0` means "no
        // timeout / wait forever" and is never woken by the tick. A queue
        // waiter woken here stays in its waiter list; it detects the timeout on
        // resume by finding itself still listed (plan W4.2/W4.3).
        for i in 0..MAX_TASKS {
            if let Some(tcb) = &mut self.tasks[i] {
                let timed = matches!(
                    tcb.state,
                    TaskState::BlockedSleep | TaskState::BlockedSend | TaskState::BlockedRecv
                );
                if timed && tcb.sleep_until != 0 && tcb.sleep_until <= now {
                    tcb.state = TaskState::Ready;
                    tcb.quantum = DEFAULT_QUANTUM_MS;
                    tcb.sleep_until = 0;
                    let prio = tcb.priority;
                    // Disjoint-field write (see `make_ready`).
                    self.ready_mask |= 1u64 << prio;
                    // W3.4: a woken higher-priority task preempts immediately.
                    if prio < cur_prio {
                        need_switch = true;
                    }
                }
            }
        }

        // A ready task at a better priority than the running one must preempt.
        //
        // Without this the two cases above are the *only* things that ever
        // request a switch, and neither covers the ordinary situation of a
        // higher-priority task simply being Ready: one fires only when a
        // *blocked* task times out, the other only when a task at the *same*
        // priority is waiting its turn. Observed on hardware as a system that
        // boots cleanly, services its interrupts, and then runs the idle task
        // forever while three higher-priority Ready tasks sit in ready_mask --
        // nothing was ever dispatched for the first time.
        if let Some(best) = self.highest_ready_priority() {
            if best < cur_prio {
                need_switch = true;
            }
        }

        // Decrement the current task's quantum; expiry triggers round-robin.
        if let Some(tcb) = &mut self.tasks[self.current as usize] {
            tcb.quantum = tcb.quantum.saturating_sub(1);
            if tcb.quantum == 0 {
                tcb.quantum = DEFAULT_QUANTUM_MS;
                if tcb.state == TaskState::Running {
                    // Only switch if another ready task shares this priority.
                    let prio = tcb.priority;
                    if self.another_ready_at(prio, self.current) {
                        need_switch = true;
                    }
                }
            }
        }

        need_switch
    }

    /// The best (numerically lowest) priority level with a runnable task, or
    /// `None` if nothing is ready.
    pub fn highest_ready_priority(&self) -> Option<u8> {
        if self.ready_mask == 0 {
            None
        } else {
            Some(self.ready_mask.trailing_zeros() as u8)
        }
    }

    fn another_ready_at(&self, prio: u8, except: u32) -> bool {
        self.tasks.iter().flatten().any(|t| {
            t.id != except && t.priority == prio && t.state == TaskState::Ready
        })
    }

    /// Pick the next task to run. Round-robin within the top ready priority.
    pub fn schedule(&mut self) -> u32 {
        for p in 0..NUM_PRIORITIES {
            if self.ready_mask & (1u64 << p) == 0 {
                continue;
            }
            // Round-robin: start scanning just after the last task dispatched
            // at this level, wrapping around.
            let start = (self.last_run[p] as usize + 1) % MAX_TASKS;
            for k in 0..MAX_TASKS {
                let i = (start + k) % MAX_TASKS;
                if let Some(tcb) = &self.tasks[i] {
                    if tcb.priority as usize == p
                        && matches!(tcb.state, TaskState::Ready | TaskState::Running)
                    {
                        self.last_run[p] = i as u32;
                        return i as u32;
                    }
                }
            }
        }
        self.current
    }

    /// Block the current task (queue/mutex/sleep), clearing its ready bit.
    pub fn block_current(&mut self, state: TaskState) {
        let prio = if let Some(tcb) = &mut self.tasks[self.current as usize] {
            tcb.state = state;
            Some(tcb.priority)
        } else {
            None
        };
        if let Some(p) = prio {
            self.recompute_ready_bit(p);
        }
    }

    pub fn block_current_on_mutex(&mut self, mutex_addr: usize) {
        let prio = if let Some(tcb) = &mut self.tasks[self.current as usize] {
            tcb.state = TaskState::BlockedMutex;
            tcb.blocked_on_mutex = Some(mutex_addr);
            Some(tcb.priority)
        } else {
            None
        };
        if let Some(p) = prio {
            self.recompute_ready_bit(p);
        }
    }

    /// Unblock a specific task.
    pub fn unblock(&mut self, id: u32) {
        let info = if let Some(tcb) = &mut self.tasks[id as usize] {
            if matches!(
                tcb.state,
                TaskState::BlockedSend
                    | TaskState::BlockedRecv
                    | TaskState::BlockedMutex
                    | TaskState::BlockedSleep
            ) {
                tcb.state = TaskState::Ready;
                tcb.quantum = DEFAULT_QUANTUM_MS;
                tcb.blocked_on_mutex = None;
                Some(tcb.priority)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(prio) = info {
            self.set_ready_bit(prio);
            // A higher-priority unblocked task should preempt.
            if prio < self.current_priority() {
                set_pending_switch();
            }
        }
    }

    /// Change a task's effective priority, moving its ready bit if necessary.
    fn set_effective_priority(&mut self, id: u32, new_prio: u8) {
        let old = if let Some(tcb) = &mut self.tasks[id as usize] {
            let old = tcb.priority;
            tcb.priority = new_prio;
            old
        } else {
            return;
        };
        if old != new_prio {
            self.recompute_ready_bit(old);
            // If the task is runnable, set its new priority bit. Compute the
            // flag first so no `self.tasks` borrow is live during the mutation.
            let active = matches!(
                self.tasks[id as usize].as_ref().map(|t| t.state),
                Some(TaskState::Ready) | Some(TaskState::Running)
            );
            if active {
                self.set_ready_bit(new_prio);
            }
        }
    }

    /// Boost `id` to at least `target` priority (lower number = higher).
    /// Records the fall-back priority on first boost (W3.2).
    pub fn boost_priority(&mut self, id: u32, target: u8) {
        // Read what we need first so no borrow is held during mutation.
        let (cur_prio, already_boosted) = match &self.tasks[id as usize] {
            Some(t) => (t.priority, t.boosted_from.is_some()),
            None => return,
        };
        if target < cur_prio {
            if !already_boosted {
                if let Some(t) = &mut self.tasks[id as usize] {
                    t.boosted_from = Some(cur_prio);
                }
            }
            self.set_effective_priority(id, target);
        }
    }

    /// Restore `id` to a specific effective priority (used when recomputing
    /// inheritance after a mutex release). `effective` is typically the max of
    /// the base priority and the highest remaining waiter.
    pub fn set_inherited_priority(&mut self, id: u32, effective: u8) {
        self.set_effective_priority(id, effective);
        if let Some(tcb) = &mut self.tasks[id as usize] {
            if effective >= tcb.base_prio {
                tcb.boosted_from = None;
            }
        }
    }

    pub fn base_priority(&self, id: u32) -> u8 {
        self.tasks[id as usize].as_ref().map_or(IDLE_PRIORITY, |t| t.base_prio)
    }
}

/// Set when a context switch is owed, consumed by the trap handler.
///
/// Deliberately a free-standing static rather than a `Scheduler` field.
/// `request_switch()` is called from task context *after* its critical section
/// has closed, so reaching this through `global()` would mint an
/// `&'static mut Scheduler` with interrupts unmasked -- racing the trap
/// handler's own reference and violating the contract `global()` documents.
/// An atomic needs no such reference.
static PENDING_SWITCH: AtomicBool = AtomicBool::new(false);

/// Record that a switch is owed.
pub fn set_pending_switch() {
    PENDING_SWITCH.store(true, Ordering::Relaxed);
}

/// Consume the pending-switch flag, returning whether one was owed.
pub fn take_pending_switch() -> bool {
    PENDING_SWITCH.swap(false, Ordering::Relaxed)
}

/// Global scheduler instance. Access only via `with()` (critical section) from
/// task context, or directly from the trap handler (interrupts already masked).
static mut SCHEDULER: Scheduler = Scheduler::new();

/// Raw access — caller MUST hold a critical section or be in the trap handler.
pub fn global() -> &'static mut Scheduler {
    unsafe { &mut *core::ptr::addr_of_mut!(SCHEDULER) }
}

/// Run `f` with the scheduler under a critical section (task-context safe).
pub fn with<R>(f: impl FnOnce(&mut Scheduler) -> R) -> R {
    flint_arch_xtensa::cs_with(|| f(global()))
}

/// Request a context switch: set the flag and raise the software interrupt so
/// the switch happens in the trap handler.
pub fn request_switch() {
    set_pending_switch();
    unsafe { flint_arch_xtensa::registers::request_switch() }
}
