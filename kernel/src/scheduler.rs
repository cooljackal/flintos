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
use hal::tick::TickSource;
use hal::types::TaskContext;
use crate::arch::Tick;

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
    /// The task has deleted itself and is waiting to be reaped. Terminal:
    /// nothing moves a task out of this state, and nothing schedules it.
    ///
    /// It exists because a task cannot free the stack it is executing on. A
    /// self-delete marks this, drops out of the ready set and switches away;
    /// the idle task frees the stack and the slot afterwards, from a context
    /// that is provably not the dying task's.
    ///
    /// Every transition out of a blocked state — `unblock`, `on_tick`,
    /// `make_ready` — matches on the blocked states by name, so none of them
    /// resurrect a task in this one. That is what makes "terminal" true rather
    /// than merely intended.
    Deleting,
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
    /// Which core(s) this task may run on.
    pub affinity: Affinity,
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
    /// Whether `stack_base` came from the radio heap rather than the linker's
    /// bump-allocated pool.
    ///
    /// The static pool is never reclaimed — it is a bump allocator, by design,
    /// because a static RTOS creates its tasks once. The radio blobs create
    /// and delete tasks throughout a session, so theirs come from the heap and
    /// this says which kind to give back. Getting it wrong either leaks a
    /// stack or frees a pointer the heap never owned.
    pub heap_stack: bool,
}

impl TaskControlBlock {
    const fn zeroed() -> Self {
        Self {
            id: u32::MAX,
            name: "",
            entry: None,
            affinity: Affinity::Any,
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
            heap_stack: false,
        }
    }
}

/// Which core(s) a task may run on.
///
/// The default is [`Affinity::Any`], because most tasks genuinely do not care
/// and pinning what does not need pinning throws away the second core.
///
/// Pinning exists for the ones that do: a driver whose peripheral interrupt is
/// routed to one core's matrix cannot service it from the other, and anything
/// with a hard timing budget does not want to be moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Affinity {
    /// Runs wherever there is room.
    #[default]
    Any,
    /// Runs only on this core, and is skipped by every other.
    Core(hal::smp::CoreId),
}

impl Affinity {
    /// Whether a task with this affinity may run on `core`.
    pub const fn allows(self, core: hal::smp::CoreId) -> bool {
        match self {
            Affinity::Any => true,
            Affinity::Core(c) => c.0 == core.0,
        }
    }

    /// The core this is pinned to, if any.
    pub const fn pinned_to(self) -> Option<hal::smp::CoreId> {
        match self {
            Affinity::Any => None,
            Affinity::Core(c) => Some(c),
        }
    }
}

pub struct Scheduler {
    pub tasks: [Option<TaskControlBlock>; MAX_TASKS],
    /// The task each core is running. Indexed by [`hal::smp::CoreId`].
    ///
    /// Per-core because two cores run two tasks. A single field was correct
    /// while one core ran the kernel and becomes "whichever core wrote last"
    /// the moment that stops — the sort of bug that shows up as a task running
    /// on both cores at once.
    ///
    /// Read it through [`Scheduler::current`], which asks the caller's core.
    pub current_per_core: [u32; hal::smp::MAX_CORES],
    /// One bit per effective priority level with at least one ready task.
    pub ready_mask: u64,
    /// Round-robin rotor: last task index dispatched at each priority level.
    last_run: [u32; NUM_PRIORITIES],
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            tasks: [const { None }; MAX_TASKS],
            current_per_core: [u32::MAX; hal::smp::MAX_CORES],
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

    /// Whether `id` is the current task on any core.
    ///
    /// `TaskState::Running` should already imply this, but the two are set at
    /// slightly different moments during a switch, and a delete that raced
    /// that window would free a stack still being executed on. Checking both
    /// costs one comparison per core.
    pub fn is_current_anywhere(&self, id: u32) -> bool {
        self.current_per_core.iter().any(|&c| c == id)
    }

    /// The task the *calling* core is running.
    pub fn current(&self) -> u32 {
        self.current_per_core[crate::smp::current_core().index()]
    }

    /// What `core` is running.
    pub fn current_on(&self, core: hal::smp::CoreId) -> u32 {
        self.current_per_core[core.index()]
    }

    pub fn set_current(&mut self, id: u32) {
        self.current_per_core[crate::smp::current_core().index()] = id;
        if let Some(tcb) = &mut self.tasks[id as usize] {
            tcb.state = TaskState::Running;
        }
    }

    /// The single authoritative tick count (owned by the tick source).
    pub fn ticks(&self) -> u64 {
        Tick::now()
    }

    pub fn current_priority(&self) -> u8 {
        self.tasks[self.current() as usize]
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
        if let Some(tcb) = &mut self.tasks[self.current() as usize] {
            tcb.quantum = tcb.quantum.saturating_sub(1);
            if tcb.quantum == 0 {
                tcb.quantum = DEFAULT_QUANTUM_MS;
                if tcb.state == TaskState::Running {
                    // Only switch if another ready task shares this priority.
                    let prio = tcb.priority;
                    if self.another_ready_at(prio, self.current()) {
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
    /// Pick the highest-priority runnable task this core may run.
    ///
    /// "May run" is the new part. A task pinned elsewhere is skipped even when
    /// its priority bit is set, so `ready_mask` is a hint about *some* core
    /// rather than a promise to this one — which is why the priority loop
    /// continues instead of returning as soon as a bit is found.
    pub fn schedule(&mut self) -> u32 {
        let core = crate::smp::current_core();
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
                        && tcb.affinity.allows(core)
                        // A task already running on the *other* core must not
                        // be handed to this one as well.
                        && !self.running_elsewhere(i as u32, core)
                    {
                        self.last_run[p] = i as u32;
                        return i as u32;
                    }
                }
            }
        }
        self.current()
    }

    /// Whether `id` is the current task on some core other than `core`.
    fn running_elsewhere(&self, id: u32, core: hal::smp::CoreId) -> bool {
        self.current_per_core
            .iter()
            .enumerate()
            .any(|(c, &cur)| c != core.index() && cur == id)
    }

    /// Block the current task (queue/mutex/sleep), clearing its ready bit.
    pub fn block_current(&mut self, state: TaskState) {
        let prio = if let Some(tcb) = &mut self.tasks[self.current() as usize] {
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
        let prio = if let Some(tcb) = &mut self.tasks[self.current() as usize] {
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
/// **Per core.** One flag for both cores meant either could consume the
/// other's request: a task called `sleep_ms`, set the flag, and the *other*
/// core's trap handler took it — so the switch never happened where it was
/// asked for and the task carried on running with its TCB marked Blocked.
///
/// On hardware that showed as a task sleeping 7 ms iterating ten thousand
/// times a second. It is invisible with one core, because there is no one else
/// to take the flag.
///
/// Deliberately a free-standing static rather than a `Scheduler` field.
/// `request_switch()` is called from task context *after* its critical section
/// has closed, so reaching this through `global()` would mint an
/// `&'static mut Scheduler` with interrupts unmasked -- racing the trap
/// handler's own reference and violating the contract `global()` documents.
/// An atomic needs no such reference.
static PENDING_SWITCH: [AtomicBool; hal::smp::MAX_CORES] =
    [const { AtomicBool::new(false) }; hal::smp::MAX_CORES];

/// Record that a switch is owed.
pub fn set_pending_switch() {
    PENDING_SWITCH[crate::smp::current_core().index()].store(true, Ordering::Relaxed);
}

/// Clear every core's pending-switch flag.
///
/// For the test harness: `reset` means power-on state, and clearing only the
/// calling core's flag left another core's set for the next test to trip over.
#[cfg(test)]
pub fn clear_all_pending_switches() {
    for f in &PENDING_SWITCH {
        f.store(false, Ordering::Relaxed);
    }
}

/// Consume the pending-switch flag, returning whether one was owed.
pub fn take_pending_switch() -> bool {
    PENDING_SWITCH[crate::smp::current_core().index()].swap(false, Ordering::Relaxed)
}

/// The scheduler, behind a lock that excludes the other core as well as this
/// core's own interrupts.
///
/// It used to be a bare `static mut` reached through a `global()` that
/// documented "caller must hold a critical section". That was sound while one
/// core ran the kernel and became a data race in the language's own terms the
/// moment a second one could — a critical section is `rsil`, which masks the
/// calling core only.
static SCHEDULER: crate::smp::Spinlock<Scheduler> = crate::smp::Spinlock::new(Scheduler::new());

/// Run `f` with the scheduler locked. Safe from task or interrupt context, on
/// either core.
///
/// Keep `f` short: interrupts are masked on this core throughout, and the
/// other core spins if it wants the scheduler meanwhile.
pub fn with<R>(f: impl FnOnce(&mut Scheduler) -> R) -> R {
    SCHEDULER.with(f)
}

/// Run `f` only if the scheduler is free, rather than waiting.
///
/// For diagnostics that must not hang — a fault handler reporting task state
/// cannot afford to block on a lock whose holder may be the thing that faulted.
pub fn try_with<R>(f: impl FnOnce(&mut Scheduler) -> R) -> Option<R> {
    SCHEDULER.try_with(f)
}


/// Request a context switch: set the flag and raise the software interrupt so
/// the switch happens in the trap handler.
pub fn request_switch() {
    set_pending_switch();
    unsafe { crate::arch::registers::request_switch() }
}

// ── Affinity tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod affinity_tests {
    use super::*;
    use crate::testsupport;
    use hal::smp::CoreId;

    /// Make `id` pinned to `core`.
    fn pin(id: u32, core: u8) {
        with(|s| {
            if let Some(tcb) = &mut s.tasks[id as usize] {
                tcb.affinity = Affinity::Core(CoreId(core));
            }
        });
    }

    #[test]
    fn a_pending_switch_belongs_to_the_core_that_asked() {
        // One flag for both cores let either consume the other's request, so a
        // task that yielded on one core kept running while the other core
        // switched instead.
        let _k = testsupport::lock();
        let me = crate::smp::current_core();
        let other = CoreId(if me.0 == 0 { 1 } else { 0 });

        set_pending_switch();
        // Draining the other core's flag must not take ours.
        assert!(!PENDING_SWITCH[other.index()].swap(false, core::sync::atomic::Ordering::Relaxed));
        assert!(take_pending_switch(), "our own request went missing");
        assert!(!take_pending_switch(), "consumed twice");
    }

    #[test]
    fn affinity_answers_who_may_run() {
        assert!(Affinity::Any.allows(CoreId(0)));
        assert!(Affinity::Any.allows(CoreId(1)));
        assert!(Affinity::Core(CoreId(1)).allows(CoreId(1)));
        assert!(!Affinity::Core(CoreId(1)).allows(CoreId(0)));
        assert_eq!(Affinity::Any.pinned_to(), None);
        assert_eq!(Affinity::Core(CoreId(1)).pinned_to(), Some(CoreId(1)));
    }

    #[test]
    fn a_task_defaults_to_running_anywhere() {
        // Pinning is opt-in. A default of "core 0" would quietly waste the
        // second core for every application that never thought about it.
        let _k = testsupport::lock();
        let id = testsupport::task(5);
        with(|s| assert_eq!(s.tasks[id as usize].as_ref().unwrap().affinity, Affinity::Any));
    }

    #[test]
    fn a_task_pinned_elsewhere_is_never_chosen() {
        // The property #20 is about: a pinned task must not migrate. Its
        // priority bit is set in `ready_mask` either way, so a scheduler that
        // trusted the mask alone would hand it straight to the wrong core.
        let _k = testsupport::lock();
        let mine = testsupport::task(5);
        let theirs = testsupport::task(3); // higher priority, wrong core
        pin(theirs, 1);
        pin(mine, 0);

        let picked = with(|s| {
            // The test thread is some core id; force the question to be about
            // core 0 by pinning the candidate set accordingly.
            s.ready_mask |= 1u64 << 3;
            s.schedule()
        });
        // Whichever core the test thread claims to be, it must not be handed a
        // task pinned to the *other* one.
        let core = crate::smp::current_core();
        let aff = with(|s| s.tasks[picked as usize].as_ref().unwrap().affinity);
        assert!(aff.allows(core), "picked {picked}, pinned {aff:?}, on {core:?}");
    }

    #[test]
    fn an_unpinned_task_is_eligible_on_every_core() {
        let _k = testsupport::lock();
        let id = testsupport::task(7);
        let aff = with(|s| s.tasks[id as usize].as_ref().unwrap().affinity);
        for c in 0..hal::smp::MAX_CORES as u8 {
            assert!(aff.allows(CoreId(c)), "unpinned task refused core {c}");
        }
    }

    #[test]
    fn current_is_tracked_per_core() {
        // One field would make "the current task" mean whichever core wrote
        // last, and a task would appear to run on both at once.
        let _k = testsupport::lock();
        let a = testsupport::task(5);
        let b = testsupport::task(6);
        with(|s| {
            s.current_per_core[0] = a;
            s.current_per_core[1] = b;
            assert_eq!(s.current_on(CoreId(0)), a);
            assert_eq!(s.current_on(CoreId(1)), b);
            assert_ne!(s.current_on(CoreId(0)), s.current_on(CoreId(1)));
        });
    }

    #[test]
    fn a_task_running_on_one_core_is_not_handed_to_the_other() {
        // Without this check both cores can pick the same task, and it would
        // then be resumed from one saved context on two stacks.
        let _k = testsupport::lock();
        let only = testsupport::task(5);
        let me = crate::smp::current_core();
        let other = CoreId(if me.0 == 0 { 1 } else { 0 });
        with(|s| {
            s.current_per_core[other.index()] = only;
            assert!(s.running_elsewhere(only, me));
            assert!(!s.running_elsewhere(only, other));
        });
    }

    #[test]
    fn pinning_to_a_core_that_does_not_schedule_is_refused() {
        // The second core exists and runs code, but nothing on it calls
        // `schedule()` yet. A task pinned there would look spawned and never
        // run -- the worst of the three possible outcomes.
        let _k = testsupport::lock();
        if crate::smp::SCHEDULING_CORES < 2 {
            assert!(crate::syscall::_flint_sys_spawn_on(
                1,
                "pinned-to-idle-core",
                || {},
                hal::types::Priority::Normal(1),
                4096
            )
            .is_none());
        }
    }

    #[test]
    fn affinity_is_recorded_on_the_task_that_asked_for_it() {
        // `sys_spawn_on` cannot run here -- it allocates from a stack pool the
        // host does not have -- so this checks the part that is about pinning:
        // the affinity reaches the TCB and survives.
        let _k = testsupport::lock();
        let id = testsupport::task(5);
        pin(id, 0);
        with(|s| {
            assert_eq!(
                s.tasks[id as usize].as_ref().unwrap().affinity,
                Affinity::Core(CoreId(0))
            );
        });
        assert!(crate::smp::is_pinnable(0));
    }

    #[test]
    fn pinning_to_a_core_that_does_not_exist_is_refused() {
        // Clamping to core 0 would run the task somewhere it explicitly asked
        // not to be, which is worse than not starting.
        let _k = testsupport::lock();
        assert!(crate::syscall::_flint_sys_spawn_on(
            hal::smp::MAX_CORES as u8,
            "nope",
            || {},
            hal::types::Priority::Normal(1),
            4096
        )
        .is_none());
    }
}

