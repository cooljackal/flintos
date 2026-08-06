// SPDX-License-Identifier: Apache-2.0

//! The Flint RTOS kernel.
//!
//! This crate is a library, not a binary. A Flint *application* is the binary:
//! it links this crate, names an entry point with [`flint_app!`], and spawns
//! its own tasks. See `apps/` for worked examples and `apps/README.md` for how
//! to start one of your own.

#![no_std]
#![feature(asm_experimental_arch)]

pub mod board;
pub mod boot;
pub mod debug;
pub mod dma_broker;
pub mod interrupt;
pub mod mutex;
#[cfg(feature = "phase0-tests")]
pub mod phase0_test;
pub mod queue;
pub mod scheduler;
pub mod spawn;
pub mod startup;
pub mod switch;
pub mod syscall;
pub mod timer;

pub use scheduler::{Scheduler, TaskState, MAX_TASKS};

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
