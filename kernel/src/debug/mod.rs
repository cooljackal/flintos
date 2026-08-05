// SPDX-License-Identifier: Apache-2.0

//! Debug and diagnostics subsystem.
//!
//! Provides kernel logging (`write`, ring buffer), console output via
//! UART, panic postmortem to SRAM, stack high-water-mark scanning,
//! and metrics stubs.

pub mod console;
pub mod fault;
pub mod log;
pub mod metrics;
pub mod panic;
pub mod stack;
