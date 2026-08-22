// SPDX-License-Identifier: Apache-2.0

//! The seam between the kernel's logic and the machine underneath it.
//!
//! On the target this is a thin re-export of `arch-xtensa`. On a host it is a
//! set of stand-ins — and those stand-ins are the only reason `cargo test` can
//! build this crate at all. `arch-xtensa` carries
//! `#![feature(asm_experimental_arch)]`, which is `E0554` on the stable
//! toolchain CI uses, so a kernel that names it unconditionally cannot be
//! compiled for the host. Its unit tests then run nowhere, which is precisely
//! where they ran before this module existed.
//!
//! The stand-ins are instrumented rather than inert. A `cs_with` that only
//! called its closure would accept an unbalanced critical section without
//! complaint; this one tracks nesting so a test can assert on it. A tick you
//! can set is what makes a rollover test possible at all: at one millisecond
//! the real counter needs longer than the age of the universe to wrap, so
//! lying about the clock is the only way to reach the arithmetic.
//!
//! **What this does not do is pretend to be a machine.** The host `cs_with`
//! masks nothing, because on a host there is nothing to mask. The concurrency
//! this kernel actually has — a timer ISR preempting a task in the middle of a
//! critical section, a register window spilled across a trap — has no host
//! equivalent, and no fake here will find a bug in it. That is what the
//! on-target suite is for (`make test-target`). This seam buys coverage of
//! *logic*: state machines, bounds, exhaustion, arithmetic. Read a green host
//! run as "the logic is consistent", never as "the kernel works".

#[cfg(all(target_os = "none", feature = "arch-armv6m"))]
struct SelectedCriticalSection;

#[cfg(all(target_os = "none", feature = "arch-armv6m"))]
critical_section::set_impl!(SelectedCriticalSection);

#[cfg(all(target_os = "none", feature = "arch-armv6m"))]
unsafe impl critical_section::Impl for SelectedCriticalSection {
    unsafe fn acquire() -> critical_section::RawRestoreState {
        unsafe { cs_enter() }
    }

    unsafe fn release(state: critical_section::RawRestoreState) {
        unsafe { cs_exit(state) }
    }
}

// ── Target: the real thing ──────────────────────────────────────────────────

#[cfg(all(target_os = "none", feature = "arch-xtensa"))]
pub use arch_xtensa::registers;
#[cfg(all(target_os = "none", feature = "arch-xtensa"))]
pub use arch_xtensa::smp::XtensaSmp as Smp;
#[cfg(all(target_os = "none", feature = "arch-xtensa"))]
pub use arch_xtensa::tick::XtensaTick as Tick;
#[cfg(all(target_os = "none", feature = "arch-xtensa"))]
pub use arch_xtensa::XtensaArch as SelectedArch;
#[cfg(all(target_os = "none", feature = "arch-xtensa"))]
pub use arch_xtensa::{cs_enter, cs_exit, cs_with};

#[cfg(all(target_os = "none", feature = "arch-armv6m"))]
pub use arch_armv6m::smp::Armv6mSmp as Smp;
#[cfg(all(target_os = "none", feature = "arch-armv6m"))]
pub use arch_armv6m::tick::Armv6mTick as Tick;
#[cfg(all(target_os = "none", feature = "arch-armv6m"))]
pub use arch_armv6m::Armv6mArch as SelectedArch;
#[cfg(all(target_os = "none", feature = "arch-armv6m"))]
pub use arch_armv6m::{cs_enter, cs_exit, cs_try_with, cs_with, init_boot_core};

// ── Host: stand-ins, with the instrumentation the real ones cannot offer ────

#[cfg(not(target_os = "none"))]
pub use host::{
    cs_enter, cs_exit, cs_with, registers, HostArch as SelectedArch, HostSmp as Smp,
    HostTick as Tick,
};

#[cfg(not(target_os = "none"))]
pub fn cs_try_with<R>(f: impl FnOnce() -> R) -> Option<R> {
    Some(host::cs_with(f))
}

#[cfg(all(target_os = "none", feature = "arch-xtensa"))]
pub fn cs_try_with<R>(f: impl FnOnce() -> R) -> Option<R> {
    Some(arch_xtensa::cs_with(f))
}

/// Saved frame selected with the architecture in exactly one place.
pub type Context = <SelectedArch as hal::arch::Architecture>::Context;

/// Park until an interrupt, through the selected architecture.
#[inline]
pub fn wait_for_interrupt() {
    <SelectedArch as hal::arch::Architecture>::wait_for_interrupt();
}

/// Terminal halt through the selected architecture.
#[inline]
pub fn wait_masked() {
    <SelectedArch as hal::arch::Architecture>::wait_masked();
}

#[cfg(not(target_os = "none"))]
pub mod host {
    //! Host stand-ins for the Xtensa primitives the kernel's logic touches.
    //!
    //! Only what the logic modules actually reach is here. `boot` and `switch`
    //! need a further dozen register accessors apiece — reading `VECBASE`, the
    //! stack pointer, `EXCCAUSE` — and faking those would be inventing a CPU.
    //! Both are `#[cfg(target_os = "none")]` in `lib.rs` instead.

    extern crate std;

    use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    pub struct HostContext;

    impl hal::arch::TaskContext for HostContext {
        const ZERO: Self = Self;
    }

    pub struct HostArch;

    impl hal::arch::Architecture for HostArch {
        type Context = HostContext;

        unsafe fn init_context(_context: &mut HostContext, _entry: usize, _stack_top: u32) {}

        unsafe fn save_context(_frame: *const HostContext, _saved: &mut HostContext) {}

        fn restore_context(saved: &mut HostContext) -> *mut HostContext {
            saved
        }

        fn request_switch() {
            SWITCH_REQUESTS.fetch_add(1, Ordering::SeqCst);
        }

        fn wait_for_interrupt() {
            panic!("wait_for_interrupt() is target-only; a host test reached hardware idle")
        }

        fn wait_masked() {
            panic!("wait_masked() is target-only; a host test reached the halt path")
        }

        fn mask_interrupts() -> u32 {
            0
        }

        fn cycle_count() -> Option<u32> {
            None
        }

        unsafe fn trap_cause(_frame: *const HostContext) -> hal::arch::TrapCause {
            panic!("trap_cause() is target-only; a host test entered a trap")
        }

        fn acknowledge_switch_request() {}

        fn context_diagnostics(_context: &HostContext) -> hal::arch::ContextDiagnostics {
            hal::arch::ContextDiagnostics {
                pc: 0,
                architecture_state: 0,
            }
        }
    }

    /// Core identity on a host.
    ///
    /// **A thread stands in for a core**, and each one gets its own id. That
    /// is what lets the spinlock be tested against genuine parallelism instead
    /// of a simulation of it.
    ///
    /// **A test must therefore use no more than `MAX_CORES` threads at once.**
    /// Ids wrap, so a third concurrent thread would share an id with the first
    /// — and two contexts sharing an id look to the spinlock like one core
    /// locking twice, which trips its reentrancy check on honest contention.
    ///
    /// Wrapping is not a shortcut, it is the constraint hardware imposes: a
    /// core id indexes per-core arrays (`Scheduler::current_per_core`), so it
    /// must be less than `MAX_CORES`. An earlier version handed out unique ids
    /// so the lock tests could use eight threads, and the scheduler promptly
    /// indexed a two-element array with core 9.
    pub struct HostSmp;

    static NEXT_CORE: AtomicU32 = AtomicU32::new(0);

    std::thread_local! {
        static MY_CORE: u8 = {
            let n = NEXT_CORE.fetch_add(1, Ordering::Relaxed) as usize;
            (n % hal::smp::MAX_CORES) as u8
        };
    }

    static NEXT_CONTEXT: AtomicU32 = AtomicU32::new(0);

    std::thread_local! {
        /// Unique per thread, unlike the core id. Capped below the spinlock's
        /// `UNLOCKED` sentinel; a test with 254 live threads has other
        /// problems.
        static MY_CONTEXT: u8 = (NEXT_CONTEXT.fetch_add(1, Ordering::Relaxed) % 254) as u8;
    }

    impl hal::smp::MultiCore for HostSmp {
        fn current_core() -> hal::smp::CoreId {
            hal::smp::CoreId(MY_CORE.with(|c| *c))
        }
        fn cores() -> u8 {
            hal::smp::MAX_CORES as u8
        }
        /// Unique per thread, so two threads never look like one core taking a
        /// lock twice. On hardware this is just the core id.
        fn context_id() -> u8 {
            MY_CONTEXT.with(|c| *c)
        }
    }

    // Per-thread, because masking is per-core and a thread stands in for a
    // core here. It was a process-global counter, which meant one test's
    // nesting was visible to every other -- and once the spinlock tests
    // started spawning eight threads that call `cs_with`, the balance
    // assertions began failing on threads that had done nothing wrong.
    std::thread_local! {
        static CS_DEPTH_TLS: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
    }
    static CS_ENTRIES: AtomicU32 = AtomicU32::new(0);
    static CS_MAX_DEPTH: AtomicU32 = AtomicU32::new(0);
    static SWITCH_REQUESTS: AtomicU32 = AtomicU32::new(0);
    static NOW: AtomicU64 = AtomicU64::new(0);

    /// Run `f` "with interrupts masked".
    ///
    /// Nothing is masked — but the nesting is counted, which the hardware
    /// version cannot report. An unbalanced critical section is invisible to a
    /// pass-through stub and to real silicon alike; here it is a number a test
    /// can read.
    #[inline]
    pub fn cs_with<R>(f: impl FnOnce() -> R) -> R {
        let depth = CS_DEPTH_TLS.with(|d| {
            d.set(d.get() + 1);
            d.get()
        });
        CS_ENTRIES.fetch_add(1, Ordering::SeqCst);
        CS_MAX_DEPTH.fetch_max(depth, Ordering::SeqCst);

        // The decrement lives in a Drop guard, not after the call, so a
        // panicking closure still balances. The counter is process-global: if
        // one panicking test left it raised, every later test asserting on
        // depth would read a corrupted value and fail somewhere unrelated.
        // This mirrors the target, where the token's Drop restores PS.
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                CS_DEPTH_TLS.with(|d| d.set(d.get() - 1));
            }
        }
        let _guard = Guard;

        f()
    }

    /// Enter a critical section without a closure, for parity with the target.
    ///
    /// Nothing is masked here either; what it does is keep the same counters
    /// `cs_with` keeps, so `cs_depth` stays truthful whichever form was used.
    ///
    /// # Safety
    /// Must be matched by exactly one [`cs_exit`]. See the target
    /// implementation for what that contract is really protecting.
    pub unsafe fn cs_enter() -> u32 {
        let depth = CS_DEPTH_TLS.with(|d| {
            d.set(d.get() + 1);
            d.get()
        });
        CS_ENTRIES.fetch_add(1, Ordering::SeqCst);
        CS_MAX_DEPTH.fetch_max(depth, Ordering::SeqCst);
        0
    }

    /// Leave a critical section entered with [`cs_enter`].
    ///
    /// # Safety
    /// `saved` must come from the matching [`cs_enter`].
    pub unsafe fn cs_exit(_saved: u32) {
        // Saturating rather than wrapping: an unbalanced exit is a bug, and a
        // panic here would blame whichever test ran next rather than the one
        // that unbalanced it. `cs_depth` still shows the imbalance.
        CS_DEPTH_TLS.with(|d| d.set(d.get().saturating_sub(1)));
    }

    /// Critical-section nesting depth right now. Zero outside any `cs_with`.
    pub fn cs_depth() -> u32 {
        CS_DEPTH_TLS.with(|d| d.get())
    }

    /// How many times `cs_with` has been entered since the last reset.
    pub fn cs_entries() -> u32 {
        CS_ENTRIES.load(Ordering::SeqCst)
    }

    /// Deepest nesting reached since the last reset.
    pub fn cs_max_depth() -> u32 {
        CS_MAX_DEPTH.load(Ordering::SeqCst)
    }

    /// How many context switches the kernel has asked for.
    pub fn switch_requests() -> u32 {
        SWITCH_REQUESTS.load(Ordering::SeqCst)
    }

    /// Set the tick clock. The point of a fake tick: reaching `u64::MAX` on
    /// real hardware is not an experiment anyone can run.
    pub fn set_now(ms: u64) {
        NOW.store(ms, Ordering::SeqCst);
    }

    /// Advance the tick clock by `ms`.
    pub fn advance(ms: u64) {
        NOW.fetch_add(ms, Ordering::SeqCst);
    }

    /// Reset every counter. Call at the top of a test that asserts on them.
    ///
    /// These are process-global, and Rust runs tests in threads by default, so
    /// a test that asserts on a counter must own it — see the note on
    /// `arch_seam` in the kernel's test modules.
    pub fn reset() {
        CS_DEPTH_TLS.with(|d| d.set(0));
        CS_ENTRIES.store(0, Ordering::SeqCst);
        CS_MAX_DEPTH.store(0, Ordering::SeqCst);
        SWITCH_REQUESTS.store(0, Ordering::SeqCst);
        NOW.store(0, Ordering::SeqCst);
    }

    /// The host tick source. Implements the same `hal::tick::TickSource` the
    /// Xtensa one does, so the kernel cannot tell them apart by type.
    pub struct HostTick;

    impl HostTick {
        /// Advance the tick counter by `ticks`, forwards only — the host twin
        /// of `XtensaTick::advance`, so the kernel's sleep reconciliation can
        /// be exercised off-target. Adds, never subtracts, so `now()` cannot
        /// regress.
        pub fn advance(ticks: u64) {
            NOW.fetch_add(ticks, Ordering::SeqCst);
        }
    }

    impl hal::tick::TickSource for HostTick {
        fn init(_period_us: u32, _cpu_hz: u32) {}

        fn tick() -> bool {
            NOW.fetch_add(1, Ordering::SeqCst);
            false
        }

        fn now() -> u64 {
            NOW.load(Ordering::SeqCst)
        }
    }

    // Deliberately no inherent `now()`. An inherent method would shadow the
    // trait one and let a call site compile on the host without importing
    // `TickSource` — then fail on the target, where only the trait provides it.
    // Resolving the same way on both is the whole point of the seam.

    /// Definitions for the symbols that `flint32.ld` and the trap assembly
    /// supply on the target.
    ///
    /// `spawn` and `debug::panic` declare these in `extern "C"` blocks and take
    /// their *addresses* — the linker script places them at the ends of the
    /// task-stack pool and at the base of the reserved panic region. On a host
    /// there is no linker script, so without definitions here the test binary
    /// does not link and every test in the crate goes unrun, which was the
    /// status quo.
    ///
    /// **These are placeholders, not a memory map.** `_task_stack_start` and
    /// `_task_stack_end` are unrelated statics whose addresses have no defined
    /// ordering, so the stack allocator's arithmetic is meaningless here and
    /// `spawn::alloc_stack` must not be called from a host test. The panic
    /// region is sized correctly (4 KiB, matching the linker script) so that
    /// `size_of::<PanicSnapshot>()` is checked against a real bound rather than
    /// an invented one.
    pub mod linker_stubs {
        /// Matches the 4 KiB `panic_region` in `arch/xtensa/flint32.ld`.
        const PANIC_REGION_WORDS: usize = 1024;

        #[no_mangle]
        static mut _panic_region_start: [u32; PANIC_REGION_WORDS] = [0; PANIC_REGION_WORDS];

        #[no_mangle]
        static _task_stack_start: u32 = 0;

        #[no_mangle]
        static _task_stack_end: u32 = 0;

        /// Matches the 8 KiB `dma_pool` in `arch/xtensa/flint32.ld`.
        ///
        /// A real array rather than two bare symbols, so a host test can check
        /// that a handed-out buffer really lands inside the pool. Two unrelated
        /// `static u32`s would give a "pool" whose size is whatever the linker
        /// happened to put between them.
        const DMA_POOL_WORDS: usize = 2048;

        #[no_mangle]
        static mut _dma_pool_start: [u32; DMA_POOL_WORDS] = [0; DMA_POOL_WORDS];

        #[no_mangle]
        static _dma_pool_end: u32 = 0;

        /// The trap-assembly entry a new task's PC is set to. Its address is
        /// stored in a task frame and jumped to by the context switcher; on a
        /// host nothing jumps anywhere, and reaching it means a test strayed
        /// off the logic and into the machine.
        #[no_mangle]
        extern "C" fn _flint_task_start() {
            panic!("_flint_task_start is target-only; a host test entered the task trampoline")
        }
    }

    /// Register-level stand-ins. Same names and signatures as the Xtensa ones,
    /// so no call site needs a `cfg`.
    pub mod registers {
        use super::{AtomicU32, Ordering};

        /// PS.WOE — Window Overflow Enable, bit 18. A bit pattern, not a
        /// machine action, so the real value is used: `spawn` writes it into
        /// the initial PS of a task frame, and a test may check that framing.
        pub const PS_WOE: u32 = 1 << 18;

        /// # Safety
        /// Sound on a host; `unsafe` matches the Xtensa signature.
        pub unsafe fn intclear(_mask: u32) {}

        /// Stand-in `INTENABLE`, so the selective-masking logic can be tested.
        ///
        /// A real one masks interrupts; this one is a number the tests can
        /// read back — which is enough to check that `mask_non_iram_safe`
        /// keeps the right bits and puts the right value back, which is the
        /// part that can be got wrong.
        static INTENABLE: AtomicU32 = AtomicU32::new(u32::MAX);

        /// # Safety
        /// Sound on a host; `unsafe` matches the Xtensa signature.
        pub unsafe fn read_intenable() -> u32 {
            INTENABLE.load(Ordering::SeqCst)
        }

        /// # Safety
        /// Sound on a host; `unsafe` matches the Xtensa signature.
        pub unsafe fn write_intenable(val: u32) {
            INTENABLE.store(val, Ordering::SeqCst);
        }

        /// # Safety
        /// Sound on a host; `unsafe` matches the Xtensa signature.
        pub unsafe fn set_intlevel_15() -> u32 {
            0
        }
    }
}

// ── Tests for the seam itself ───────────────────────────────────────────────
//
// A stub nobody checks is a stub that lies. These run only on the host, which
// is the only place the stubs exist.

#[cfg(all(test, not(target_os = "none")))]
mod tests {
    use super::host;

    #[test]
    fn cs_with_returns_the_closure_value() {
        assert_eq!(host::cs_with(|| 42), 42);
    }

    #[test]
    fn cs_with_balances_its_nesting() {
        // The property the hardware version cannot report on: enter and leave
        // must cancel out. If this stub ever stopped decrementing, every other
        // test asserting on depth would quietly pass forever.
        let before = host::cs_depth();
        host::cs_with(|| {
            assert!(host::cs_depth() > before, "depth must rise inside");
        });
        assert_eq!(host::cs_depth(), before, "depth must return to its start");
    }

    #[test]
    fn cs_with_nests() {
        let base = host::cs_depth();
        host::cs_with(|| {
            host::cs_with(|| {
                assert_eq!(host::cs_depth(), base + 2);
            });
            assert_eq!(host::cs_depth(), base + 1);
        });
        assert_eq!(host::cs_depth(), base);
    }

    #[test]
    fn cs_with_unwinds_to_a_balanced_depth_on_panic() {
        // A panicking closure must not leave the depth raised. This is the one
        // case where the stub could corrupt every later assertion in the
        // process, since the counter is global.
        let base = host::cs_depth();
        let caught = std::panic::catch_unwind(|| {
            host::cs_with(|| panic!("boom"));
        });
        assert!(caught.is_err(), "the panic must propagate");
        assert_eq!(
            host::cs_depth(),
            base,
            "a panic inside a critical section must still balance it"
        );
    }

    #[test]
    fn ps_woe_matches_the_hardware_bit() {
        // The one host constant with a real value. If it drifted from the
        // Xtensa definition, task frames built in a host test would be framed
        // differently from the ones on silicon, and the test would be
        // rehearsing fiction.
        assert_eq!(host::registers::PS_WOE, 1 << 18);
    }
}
