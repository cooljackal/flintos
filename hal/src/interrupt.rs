// SPDX-License-Identifier: Apache-2.0

//! The one interrupt type a driver or application names: which CPU input a
//! peripheral was wired to.
//!
//! The kernel's `interrupt::connect` picks the input and hands this back;
//! `interrupt::connect_at` takes one. It is a newtype rather than a bare `u8`
//! because the other number in the same call — the peripheral *source* — is
//! also a `u8`, and swapping the two compiles, routes nothing useful, and is
//! found on a board rather than in a type error.

/// One of the core's interrupt inputs. On the ESP32 an index into the 32
/// Xtensa interrupts; on a Cortex-M an NVIC vector number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CpuInt(pub u8);

impl CpuInt {
    /// The raw input number, for a register that wants it.
    pub const fn number(self) -> u8 {
        self.0
    }
}

impl core::fmt::Display for CpuInt {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "CPU interrupt {}", self.0)
    }
}
