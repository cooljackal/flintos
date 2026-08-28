// SPDX-License-Identifier: Apache-2.0

//! FlintOS public API crate.
//!
//! This crate provides the user-facing primitives:
//! - `api::task::spawn`, `Task` builder, `exit`, `sleep_ms`, `yield_now`, `wait_until`
//! - `api::queue::Queue<T,N>` — bounded, thread-safe message queue
//! - `api::mutex::Mutex<T>` — priority-inheritance mutex
//! - `api::timer::now_ms`, `once_ms`, `every_ms`
//! - `api::log_info!`, `log_error!`, etc. — kernel logging macros
//! - `api::Counter`, `api::Gauge` — named metrics
//!
//! All kernel-interactive operations use `extern "Rust"` syscall stubs
//! resolved at link time against the kernel crate.

#![no_std]

/// The application-facing ABI version.
///
/// Bumped whenever the surface an application compiles against changes
/// incompatibly: a signature in `api`, a `hal` type an application names, the
/// `flint_app!` contract, or the shape of a board manifest.
///
/// Applications declare the version they were written against
/// (`flint_app!(main, abi = N)`), and the kernel refuses to build against a
/// declaration it does not match. FlintOS moves fast and applications live in
/// their own crates, so `git pull` routinely updates the kernel underneath one
/// written against something older. Without this the result is a type error
/// somewhere in the application — or worse, a silent change in behaviour.
///
/// Every bump needs a **Breaking** entry in `CHANGELOG.md` saying what to
/// change. A bump without one is worse than no version at all: it tells a user
/// they are broken and not how to stop being broken.
pub const ABI: u32 = 2;

pub mod debug;
pub mod dma;
pub mod interrupt;
#[cfg(feature = "task-isolation")]
pub mod isolated;
pub mod mutex;
pub mod prelude;
pub mod queue;
pub mod smp;
pub mod sync;
pub mod task;
pub mod time;
pub mod timer;

/// Re-export key HAL types for convenient access.
pub use hal::types::{Priority, TaskId};

/// The one error type an application `?`s into, and its `Result` (#103).
pub use hal::{Error, Result};

/// Re-export the shared-static cells at crate root.
pub use sync::{CsCell, Once};

/// Re-export the bus surface so Layer-2/Layer-3 drivers depend only on
/// `api` (plan W7.1). Logical/bus driver crates must NOT depend on
/// `hal` or `arch-*` directly — the dependency graph is the layer
/// boundary, checked in CI by `tools/check-layers.sh`.
pub use hal::bus;
pub use hal::bus::{
    Bus, BusError, BusHandle, BusResult, BusSpeed, CsHold, Op, PhysicalBus, PhysicalTransfer,
};

/// Shared test-only bus mocks ([`testing::RegBus`], [`testing::WriteLog`]) for
/// device-driver unit tests. Enabled by the `test-support` feature.
#[cfg(feature = "test-support")]
pub use hal::testing;

/// Byte-stream subsystem surface (UART), re-exported like `bus` so Layer-2/3
/// drivers depend only on `api`. A UART is a stream, not a `Bus` — see
/// `hal::stream`.
pub use hal::stream;
pub use hal::stream::{ByteStream, StreamErrors};

/// USB packet-controller contract for portable device/class transports.
pub use hal::usb;

/// Display transport contract, re-exported from `hal::display` the same way
/// `bus` and `stream` are — so a Layer-3 panel driver names it through `api`.
pub use hal::display;
pub use hal::display::{DisplayError, DisplayInterface};

/// Pin routing, re-exported so an application that configures a pin depends
/// on `api` alone (#105). This was the one surface `api` did not carry, and
/// the only reason application manifests listed `hal`.
pub use hal::pinmux;
pub use hal::pinmux::{PinConfig, PinMux, Signal};

/// Wi-Fi station interface, re-exported from `hal::wifi` the same way `bus` is.
/// A radio backend (blob or pure-Rust) implements `api::wifi::Station`.
pub use hal::wifi;
pub use hal::wifi::{
    ApInfo, ConnectRequest, Credentials, DisconnectReason, ScanRequest, Security, Ssid, Station,
    StationEvent, StationState, StationStatus, WifiError, WifiResult,
};

/// Re-export metrics types at crate root.
pub use debug::metrics::{Counter, Gauge};
