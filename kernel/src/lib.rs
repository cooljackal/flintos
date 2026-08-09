// SPDX-License-Identifier: Apache-2.0

//! The Flint RTOS kernel.
//!
//! This crate is a library, not a binary. A Flint *application* is the binary:
//! it links this crate, names an entry point with [`flint_app!`], and spawns
//! its own tasks. See `apps/` for worked examples and `apps/README.md` for how
//! to start one of your own.

// `std` only under `cfg(test)`, and only ever on a host: the test harness needs
// it, the kernel does not. Without this the crate cannot host a `#[test]` at
// all, which is why its unit tests never ran anywhere.
#![cfg_attr(not(test), no_std)]
// Xtensa inline assembly needs an unstable feature. Gating it on the target
// keeps the host build on stable, where a bare `#![feature]` is E0554.
#![cfg_attr(target_os = "none", feature(asm_experimental_arch))]

pub mod arch;
pub mod board;
pub mod debug;
pub mod dma_broker;
pub mod nvs;
pub mod interrupt;
pub mod mutex;

// Target-only modules.
//
// These are not gated to keep the host build tidy — they genuinely cannot be
// faked. `boot` reads VECBASE, the stack pointer and PS to set the machine up;
// `switch` reads EXCCAUSE and the interrupt registers to dispatch a trap.
// Standing in for those is not stubbing a call, it is inventing a CPU, and a
// test against an invented CPU tells you about the invention. They are covered
// on real silicon by the on-target suite instead — see `make test-target`.
#[cfg(target_os = "none")]
pub mod boot;
#[cfg(target_os = "none")]
pub mod switch;
#[cfg(target_os = "none")]
pub mod watchdog;

/// The chip's hardware random number generator.
///
/// Re-exported rather than wrapped: there is nothing to add, and a wrapper
/// would only put distance between the caller and the entropy caveat in its
/// docs -- which is the part that matters.
#[cfg(target_os = "none")]
pub use esp32_rng as rng;

#[cfg(all(feature = "self-test", target_os = "none"))]
pub mod selftest;
pub mod queue;
pub mod scheduler;
pub mod smp;

#[cfg(test)]
mod testsupport;
pub mod spawn;
pub mod startup;
pub mod syscall;
pub mod timer;

pub use scheduler::{Scheduler, TaskState, MAX_TASKS};

/// The application-facing ABI this kernel provides, re-exported so
/// `flint_app!` can check it without the application naming `api` itself.
pub use api::ABI;

// ── Panic handler ───────────────────────────────────────────────────────────
//
// Lives here rather than in each application: there must be exactly one in the
// binary, and making every application supply its own would be a papercut with
// no upside. Only compiled for the bare-metal target, so host `cargo test` of
// any crate that pulls this one in still links against std's.

#[cfg(all(target_os = "none", not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Pass the location through. For a `panic!` in application code the file
    // and line are the whole diagnosis, and they go into the postmortem
    // snapshot so the *next* boot can report them too.
    let msg = format_args!("{}", info.message());
    crate::debug::panic::handle_at(&msg, info.location())
}
