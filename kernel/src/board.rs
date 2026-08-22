// SPDX-License-Identifier: Apache-2.0

//! Board manifest integration.
//!
//! Re-exports the board manifest and provides runtime initialisation
//! of board-level resources (buses, devices, peripherals).
//!
//! The concrete board is selected by a Cargo feature on `board`
//! (forwarded through this crate's own `board-*` features — see
//! `kernel/Cargo.toml`), not named here. This module just forwards
//! whichever manifest `board` resolved to, so switching boards never
//! touches kernel source:
//!
//! ```text
//! cargo build -p kernel --no-default-features --features board-m5-atom
//! ```

#[cfg(any(feature = "soc-esp32", feature = "soc-rp2040"))]
pub use board::active;

/// SoC selected by the active ESP32 board family.
#[cfg(feature = "soc-esp32")]
pub type SelectedSoc = soc_esp32::Esp32;

/// SoC selected by the Wio RP2040 Mini board family.
#[cfg(feature = "soc-rp2040")]
pub type SelectedSoc = soc_rp2040::Rp2040;
