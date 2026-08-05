//! Flint RTOS public API crate.
//!
//! This crate provides the user-facing primitives:
//! - `flint_api::task::spawn`, `sleep_ms`, `yield_now`
//! - `flint_api::queue::Queue<T,N>` — bounded, thread-safe message queue
//! - `flint_api::mutex::Mutex<T>` — priority-inheritance mutex
//! - `flint_api::timer::now_ms`, `once_ms`, `every_ms`
//! - `flint_api::log_info!`, `log_error!`, etc. — kernel logging macros
//! - `flint_api::Counter`, `flint_api::Gauge` — named metrics
//!
//! All kernel-interactive operations use `extern "Rust"` syscall stubs
//! resolved at link time against the kernel crate.

#![no_std]

pub mod debug;
pub mod mutex;
pub mod queue;
pub mod task;
pub mod timer;

/// Re-export key HAL types for convenient access.
pub use flint_hal::types::{Priority, TaskId};

/// Re-export the bus surface so Layer-2/Layer-3 drivers depend only on
/// `flint-api` (plan W7.1). Logical/bus driver crates must NOT depend on
/// `flint-hal` or `flint-arch-*` directly — the dependency graph is the layer
/// boundary, checked in CI by `tools/check-layers.sh`.
pub use flint_hal::bus;
pub use flint_hal::bus::{Bus, BusError, BusHandle, BusResult, BusSpeed, PhysicalBus};

/// Re-export metrics types at crate root.
pub use debug::metrics::{Counter, Gauge};