// SPDX-License-Identifier: Apache-2.0

//! Everything an application normally needs, in one `use`.
//!
//! ```ignore
//! use api::prelude::*;
//! ```
//!
//! Plain re-exports and nothing else (`doc/plan-ergonomics.md` §1.3): the
//! logging macros, the `task` and `timer` modules, the task builder and the
//! few free functions a task calls, the shared-static cells, the error type,
//! and the bus, pin-routing and byte-stream surfaces an application names
//! when it talks to a driver. Anything rarer stays at its full path.

pub use crate::{log_debug, log_error, log_info, log_trace, log_warn};
pub use crate::{task, timer};

pub use crate::task::{exit, sleep_ms, spawn, wait_until, Task};
pub use crate::{CsCell, Once};
pub use crate::{Error, Priority, Result, TaskId};

pub use crate::bus::{
    Bus, BusConfig, BusError, BusHandle, BusSpeed, CsHold, Op, PhysicalBus, SpiMode,
};
pub use crate::pinmux::{PinConfig, PinMux, Signal};
pub use crate::stream::ByteStream;
