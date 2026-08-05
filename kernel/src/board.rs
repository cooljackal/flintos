// SPDX-License-Identifier: Apache-2.0

//! Board manifest integration.
//!
//! Re-exports the board manifest and provides runtime initialisation
//! of board-level resources (buses, devices, peripherals).

pub use flint_board::esp32_wrover as active;
