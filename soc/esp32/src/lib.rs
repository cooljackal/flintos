// SPDX-License-Identifier: Apache-2.0

//! ESP32 SoC support.
//!
//! This crate owns what is true of the *chip* rather than of the board or of
//! the CPU core:
//!
//! - [`addr`] — peripheral base addresses and interrupt source numbers
//! - [`dport`] — peripheral clock gating and reset
//! - [`io_mux`] — the pad configuration registers and their non-linear
//!   offset table
//! - [`gpio_matrix`] — signal routing, and the signal index map
//! - [`pinmux`] — [`Esp32PinMux`], the chip's implementation of
//!   [`hal::PinMux`]
//!
//! It sits between the arch layer (Xtensa LX6: traps, context switch, tick,
//! which the ESP32 shares with other Xtensa parts) and the board layer (which
//! pin is wired to what, which differs between two boards carrying the same
//! chip). Before this split existed, board manifests carried peripheral base
//! addresses and every physical driver kept its own private copy of the
//! IO_MUX offset table.
//!
//! Everything here is specific to the *classic* ESP32 (and the PICO-D4 SiP
//! built on it). The S2, S3 and C3 have different peripheral maps and, in the
//! C3's case, a different core entirely; they get their own crates.

#![no_std]

pub mod addr;
pub mod dport;
pub mod gpio_matrix;
pub mod io_mux;
pub mod pinmux;

pub use pinmux::Esp32PinMux;

/// The chip's IO_MUX-capable GPIO count. Pins 0-39 exist; 34-39 are
/// input-only, and 20, 24, 28-31 are not bonded out at all.
pub const MAX_GPIO: u8 = 39;

/// APB clock, which every peripheral in this crate is timed against.
///
/// Fixed at 80 MHz regardless of the CPU frequency: the ESP32's CPU clock can
/// be 80, 160 or 240 MHz, but APB stays at 80 unless the chip is put into a
/// low-power mode this kernel does not use. Baud-rate and I2C divisors derive
/// from this, not from the measured CPU frequency.
pub const APB_HZ: u32 = 80_000_000;
