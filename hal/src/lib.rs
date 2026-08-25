// SPDX-License-Identifier: Apache-2.0

//! FlintOS Hardware Abstraction Layer.
//!
//! Defines the trait interfaces for all hardware-facing abstractions:
//! buses, tick timers, pin routing, MPU, and critical sections.
//! This crate contains no implementations — only the contracts that
//! architecture and driver crates must fulfil.
//!
//! **32-bit targets only.** Peripheral addresses and task-stack addresses in
//! the contracts are `u32`. Saved register-frame layouts belong to architecture
//! crates through [`arch::Architecture`].

#![no_std]

pub mod arch;
pub mod bus;
pub mod clock;
pub mod critical_section;
pub mod display;
pub mod dma;
pub mod error;
pub mod flash;
pub mod interrupt;
pub mod mpu;
pub mod pinmux;
pub mod power;
pub mod reset;
pub mod smp;
pub mod soc;
pub mod stream;
pub mod tick;
pub mod timer;
pub mod types;
pub mod wifi;

pub use arch::{Architecture, TaskContext};
pub use bus::*;
pub use clock::MonotonicClock;
pub use critical_section::CriticalSection;
pub use display::{DisplayError, DisplayInterface};
pub use dma::{DmaError, DmaHandle, DmaReach, DmaTransferId};
pub use error::{ConnectError, Error, Result};
pub use interrupt::CpuInt;
pub use mpu::MpuManager;
pub use pinmux::{PinConfig, PinDrive, PinMux, PinPull, Signal, SignalDirection};
pub use reset::PanicRecovery;
pub use smp::{CoreId, MultiCore, MAX_CORES};
pub use soc::{SocCapabilities, SystemOnChip};
pub use stream::{ByteStream, StreamErrors};
pub use tick::TickSource;
pub use types::*;
