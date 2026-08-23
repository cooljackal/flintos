// SPDX-License-Identifier: Apache-2.0

//! Which core the caller is running on.
//!
//! FlintOS runs the scheduler on more than one core, and a handful of things
//! are per core: a peripheral interrupt is routed to one core's matrix, a task
//! may be pinned. [`current_core`] answers "where am I" for code that has to
//! make such a decision at runtime.

pub use hal::CoreId;

/// The core the caller is running on.
///
/// Reads a per-core register, so it is valid from task or trap context and
/// changes across a migration — hold the result no longer than the work that
/// depends on staying put.
pub fn current_core() -> CoreId {
    extern "Rust" {
        fn _flint_sys_current_core() -> u8;
    }
    CoreId(unsafe { _flint_sys_current_core() })
}
