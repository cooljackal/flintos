// SPDX-License-Identifier: Apache-2.0

//! Debug and diagnostics subsystem.
//!
//! Provides kernel logging (`write`, ring buffer), console output over UART,
//! panic postmortem to SRAM, and stack high-water-mark scanning.

pub mod console;
pub mod fault;
pub mod log;
pub mod panic;
pub mod stack;
