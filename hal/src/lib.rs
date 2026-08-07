// SPDX-License-Identifier: Apache-2.0

//! Flint RTOS Hardware Abstraction Layer.
//!
//! Defines the trait interfaces for all hardware-facing abstractions:
//! buses, tick timers, pin routing, MPU, and critical sections.
//! This crate contains no implementations — only the contracts that
//! architecture and driver crates must fulfil.
//!
//! **32-bit targets only.** This is where the assumption becomes concrete:
//! [`types::TaskContext`] is a `#[repr(C)]` of `u32` fields that the trap entry
//! indexes by fixed byte offset, and every peripheral address in the bus traits
//! is a `u32`. A 64-bit port would rewrite both rather than widen them.

#![no_std]

pub mod bus;
pub mod critical_section;
pub mod mpu;
pub mod pinmux;
pub mod smp;
pub mod tick;
pub mod types;

pub use bus::*;
pub use critical_section::CriticalSection;
pub use mpu::MpuManager;
pub use pinmux::{PinConfig, PinDrive, PinMux, PinPull, Signal, SignalDirection};
pub use smp::{CoreId, MultiCore, MAX_CORES};
pub use tick::TickSource;
pub use types::*;
