// SPDX-License-Identifier: Apache-2.0

#![no_std]
#![feature(asm_experimental_arch)]

pub mod board;
pub mod counters;
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

// Re-export for use by the kernel binary.
pub use scheduler::{Scheduler, TaskState, MAX_TASKS};
