//! Board manifest for Flint RTOS.
//!
//! Each supported board is a submodule that exports:
//! - `TARGET_BUSES` — physical bus definitions
//! - `TARGET_DEVICES` — logical device attachments
//! - `TARGET_PERIPHERALS` — direct peripheral mappings
//! - `TARGET_SERVICES` — system service tasks

#![no_std]

pub mod esp32_wrover;
