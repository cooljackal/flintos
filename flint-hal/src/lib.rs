//! Flint RTOS Hardware Abstraction Layer.
//!
//! Defines the trait interfaces for all hardware-facing abstractions:
//! buses, syscalls, tick timers, MPU, storage, and critical sections.
//! This crate contains no implementations — only the contracts that
//! architecture and driver crates must fulfil.

#![no_std]

pub mod bus;
pub mod critical_section;
pub mod mpu;
pub mod storage;
pub mod syscall;
pub mod tick;
pub mod types;

pub use bus::*;
pub use critical_section::CriticalSection;
pub use mpu::MpuManager;
pub use storage::*;
pub use syscall::SyscallABI;
pub use tick::TickSource;
pub use types::*;
