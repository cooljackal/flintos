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

// ── Target: the real thing ──────────────────────────────────────────────────

#[cfg(target_os = "none")]
pub use arch_xtensa::cs_with;
#[cfg(target_os = "none")]
pub use arch_xtensa::registers;
#[cfg(target_os = "none")]
pub use arch_xtensa::tick::XtensaTick as Tick;

/// Park the CPU until the next interrupt.
///
/// Always called inside a `loop`; it is one instruction, not a loop itself.
#[cfg(target_os = "none")]
#[inline]
pub fn wait_for_interrupt() {
    unsafe { core::arch::asm!("waiti 0") };
}

/// Park the CPU with every maskable interrupt masked — a terminal halt.
#[cfg(target_os = "none")]
#[inline]
pub fn wait_masked() {
    unsafe { core::arch::asm!("waiti 15") };
}

// ── Host: stand-ins, with the instrumentation the real ones cannot offer ────

#[cfg(not(target_os = "none"))]
pub use host::{cs_with, registers, HostTick as Tick};

/// On a host these are only reachable by mistake: every caller is a terminal
/// `loop` in code that only runs on hardware (the idle task, the fault
/// handler, the panic halt). Returning would spin a CI job until it timed out,
/// so say what happened instead. A hang is the worst failure mode there is —
/// it reports nothing and costs a runner.
// Returning `()` rather than `!`, deliberately. Both are called as
// `loop { wait_for_interrupt(); }`, and a diverging return type turns that into
// a loop clippy correctly reports as never looping — on the host build only,
// for a construct that is right on the target. Matching the Xtensa signature
// exactly keeps the call sites identical on both.
#[cfg(not(target_os = "none"))]
#[inline]
pub fn wait_for_interrupt() {
    panic!("wait_for_interrupt() is target-only; a host test reached hardware idle")
}

#[cfg(not(target_os = "none"))]
#[inline]
pub fn wait_masked() {
    panic!("wait_masked() is target-only; a host test reached the halt path")
}

#[cfg(not(target_os = "none"))]
pub mod host {
    //! Host stand-ins for the Xtensa primitives the kernel's logic touches.
    //!
    //! Only what the logic modules actually reach is here. `boot` and `switch`
    //! need a further dozen register accessors apiece — reading `VECBASE`, the
    //! stack pointer, `EXCCAUSE` — and faking those would be inventing a CPU.
    //! Both are `#[cfg(target_os = "none")]` in `lib.rs` instead.

    use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    static CS_DEPTH: AtomicU32 = AtomicU32::new(0);
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
        let depth = CS_DEPTH.fetch_add(1, Ordering::SeqCst) + 1;
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
                CS_DEPTH.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let _guard = Guard;

        f()
    }

    /// Critical-section nesting depth right now. Zero outside any `cs_with`.
    pub fn cs_depth() -> u32 {
        CS_DEPTH.load(Ordering::SeqCst)
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
        CS_DEPTH.store(0, Ordering::SeqCst);
        CS_ENTRIES.store(0, Ordering::SeqCst);
        CS_MAX_DEPTH.store(0, Ordering::SeqCst);
        SWITCH_REQUESTS.store(0, Ordering::SeqCst);
        NOW.store(0, Ordering::SeqCst);
    }

    /// The host tick source. Implements the same `hal::tick::TickSource` the
    /// Xtensa one does, so the kernel cannot tell them apart by type.
    pub struct HostTick;

    impl hal::tick::TickSource for HostTick {
        fn init(_period_us: u32) {}

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
        use super::{Ordering, SWITCH_REQUESTS};

        /// PS.WOE — Window Overflow Enable, bit 18. A bit pattern, not a
        /// machine action, so the real value is used: `spawn` writes it into
        /// the initial PS of a task frame, and a test may check that framing.
        pub const PS_WOE: u32 = 1 << 18;

        /// Record a context-switch request instead of raising a software
        /// interrupt.
        ///
        /// # Safety
        /// Sound on a host. `unsafe` only to match the Xtensa signature.
        pub unsafe fn request_switch() {
            SWITCH_REQUESTS.fetch_add(1, Ordering::SeqCst);
        }

        /// # Safety
        /// Sound on a host; `unsafe` matches the Xtensa signature.
        pub unsafe fn intclear(_mask: u32) {}

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
