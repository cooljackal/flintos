// SPDX-License-Identifier: Apache-2.0

//! The FlintOS kernel.
//!
//! This crate is a library, not a binary. A FlintOS *application* is the binary:
//! it links this crate, names an entry point with [`flint_app!`], and spawns
//! its own tasks. See `apps/` for worked examples and `apps/README.md` for how
//! to start one of your own.

// `std` only under `cfg(test)`, and only ever on a host: the test harness needs
// it, the kernel does not. Without this the crate cannot host a `#[test]` at
// all, which is why its unit tests never ran anywhere.
#![cfg_attr(not(test), no_std)]
// Xtensa inline assembly needs an unstable feature. Gating it on the target
// keeps the host build on stable, where a bare `#![feature]` is E0554.
#![cfg_attr(
    all(target_os = "none", feature = "arch-xtensa"),
    feature(asm_experimental_arch)
)]

#[cfg(any(
    all(feature = "arch-xtensa", feature = "arch-armv6m"),
    not(any(feature = "arch-xtensa", feature = "arch-armv6m"))
))]
compile_error!("select exactly one kernel architecture: arch-xtensa or arch-armv6m");

#[cfg(any(
    all(feature = "soc-esp32", feature = "soc-rp2040"),
    not(any(feature = "soc-esp32", feature = "soc-rp2040"))
))]
compile_error!("select exactly one kernel SoC: soc-esp32 or soc-rp2040");

#[cfg(any(
    all(feature = "arch-xtensa", feature = "soc-rp2040"),
    all(feature = "arch-armv6m", feature = "soc-esp32")
))]
compile_error!("unsupported architecture/SoC pair");

pub mod arch;
pub mod board;
pub mod debug;
pub mod dma_broker;
pub mod dynobj;
pub mod heap;
pub mod interrupt;
pub mod mutex;
#[cfg(feature = "soc-esp32")]
pub mod nvs;
#[cfg(feature = "soc-esp32")]
pub mod radio;

// Target-only modules.
//
// These are not gated to keep the host build tidy — they genuinely cannot be
// faked. `boot` reads VECBASE, the stack pointer and PS to set the machine up;
// `switch` reads EXCCAUSE and the interrupt registers to dispatch a trap.
// Standing in for those is not stubbing a call, it is inventing a CPU, and a
// test against an invented CPU tells you about the invention. They are covered
// on real silicon by the on-target suite instead — see `make test-target`.
#[cfg(all(target_os = "none", feature = "soc-esp32"))]
pub mod boot;
#[cfg(all(target_os = "none", feature = "soc-esp32"))]
pub mod switch;
#[cfg(all(target_os = "none", feature = "soc-esp32"))]
pub mod watchdog;

/// The chip's hardware random number generator.
///
/// Re-exported rather than wrapped: there is nothing to add, and a wrapper
/// would only put distance between the caller and the entropy caveat in its
/// docs -- which is the part that matters.
#[cfg(all(target_os = "none", feature = "soc-esp32"))]
pub use esp32_rng as rng;

#[cfg(feature = "soc-esp32")]
pub mod alarm;
#[cfg(feature = "soc-esp32")]
pub mod clock;
pub mod power;
pub mod queue;
pub mod scheduler;
#[cfg(all(feature = "self-test", target_os = "none", feature = "soc-esp32"))]
pub mod selftest;
pub mod smp;

pub mod spawn;
pub mod startup;
pub mod syscall;
#[cfg(test)]
mod testsupport;
pub mod timer;

pub use scheduler::{Scheduler, TaskState, MAX_TASKS};

// An architecture skeleton must still compile the portable kernel surface.
// This names representative scheduling, task, and synchronization APIs so
// gating all modules away cannot make an ARM check pass vacuously.
#[cfg(feature = "arch-armv6m")]
mod arm_api_smoke {
    use super::{smp, Scheduler, TaskState, MAX_TASKS};

    const _: usize = MAX_TASKS;

    fn scheduler_and_task_surface(scheduler: &mut Scheduler, state: TaskState) {
        scheduler.block_current(state);
        let lock = smp::Spinlock::new(0u8);
        lock.with(|value| *value += 1);
    }

    const _: fn(&mut Scheduler, TaskState) = scheduler_and_task_surface;
}

/// The application-facing ABI this kernel provides, re-exported so
/// `flint_app!` can check it without the application naming `api` itself.
pub use api::ABI;

// ── Panic handler ───────────────────────────────────────────────────────────
//
// Lives here rather than in each application: there must be exactly one in the
// binary, and making every application supply its own would be a papercut with
// no upside. Only compiled for the bare-metal target, so host `cargo test` of
// any crate that pulls this one in still links against std's.

#[cfg(all(target_os = "none", feature = "soc-esp32", not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Pass the location through. For a `panic!` in application code the file
    // and line are the whole diagnosis, and they go into the postmortem
    // snapshot so the *next* boot can report them too.
    let msg = format_args!("{}", info.message());
    crate::debug::panic::handle_at(&msg, info.location())
}
