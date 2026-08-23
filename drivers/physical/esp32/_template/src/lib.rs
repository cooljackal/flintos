// SPDX-License-Identifier: Apache-2.0

//! Skeleton Layer-1 esp32 driver — copy this, don't ship it.
//!
//! This crate is not linked into any image. It exists so a new physical-driver
//! author has one small, compiling example of every convention the tree
//! settled on. The prose lives in `drivers/README.md`; this is the same thing
//! said in code. Read them together.
//!
//! The peripheral is fictional: a single 32-bit "scratch" register you can
//! write and read back. That is enough to show the shape and nothing more, so
//! the shape is all you see.
//!
//! # The conventions, and where each one is below
//!
//! - **One constructor: [`Esp32Scratch::open`]`(&`[`ScratchPort`]`) ->
//!   `[`hal::Result`]`<Self>`.** It claims the controller exactly once (a
//!   static [`AtomicBool`]), brings the hardware up, and hands back an owned
//!   handle. A second `open` returns an error rather than a second alias to
//!   the same registers.
//! - **`unsafe fn new(base)` only for self-tests.** The on-target self-test
//!   harness needs to point a driver at loopback pads or a scratch address
//!   without the claim; nothing else may call it.
//! - **Registers through [`soc_esp32::reg`]**, never a hand-rolled
//!   `fn reg(&self, off)`. `reg::at` turns base + offset into a pointer;
//!   `reg::{read,write,modify,set,clear}` do the access. See
//!   `drivers/README.md` for the seven drivers that still carry the private
//!   copy this avoids.
//! - **The driver's own error type, with `From` into [`hal::Error`]** — see
//!   [`ScratchError`] and its `impl` — so an application can `?` a driver call
//!   into its one error type.
//! - **Pad routing lives in `open`, never in the caller.** This scratch block
//!   has no pads, so there is nothing to route; a peripheral with pins routes
//!   them here through `Esp32PinMux`, exactly as `esp32-i2c` does. See
//!   `drivers/README.md`.

#![no_std]

use core::sync::atomic::{AtomicBool, Ordering};

use soc_esp32::reg;

/// Base address of the fictional scratch block.
///
/// Not a real ESP32 peripheral — the value only has to be a plausible,
/// word-aligned address in the peripheral window, because nothing dereferences
/// it except on target. A real driver takes its base from its controller enum
/// (`I2cCtrl::base()`, and friends in `soc_esp32::ctrl`), never a literal.
const SCRATCH_BASE: u32 = 0x3FF5_5000;

/// The one register, at offset 0 from the base.
const SCRATCH_VALUE: u32 = 0x00;

/// How to bring the scratch block up.
///
/// Mirrors the `*Port` structs in `soc_esp32::ctrl` (`I2cPort`, `SpiPort`,
/// `UartPort`): one `Copy` value that carries everything `open` needs, so a
/// board can name it as a `const` and an app passes it by reference. A real
/// port also names its controller and pin configuration; this one only has a
/// value to seed the register with.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ScratchPort {
    /// Value written to the register during bring-up.
    pub seed: u32,
}

/// What can go wrong bringing this driver up.
///
/// Small and specific — a driver says precisely what failed. The `From` below
/// is what lets an application `?` it into `hal::Error` without a `map_err`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScratchError {
    /// `open` was already called; the controller is owned elsewhere.
    InUse,
}

// ── Into the application's one error type ────────────────────────────────────
//
// Kept ahead of the test module, matching the rest of the tree (commit
// 803bdca moved every driver's `From` impl here for consistency).

impl From<ScratchError> for hal::Error {
    fn from(e: ScratchError) -> Self {
        match e {
            // Nothing more specific fits; map to `Other` with a static message
            // for the log. A driver whose failure matches a richer `hal::Error`
            // variant (`Unsupported`, `WrongDevice`, ...) uses that instead.
            ScratchError::InUse => hal::Error::Other("esp32 scratch already open"),
        }
    }
}

/// Set once by [`Esp32Scratch::open`], cleared when the handle is dropped.
/// This is what makes the controller single-owner.
static CLAIMED: AtomicBool = AtomicBool::new(false);

/// Take exclusive ownership of `flag`, or report it already taken.
///
/// Split out from [`Esp32Scratch::open`] with the flag as a parameter so the
/// claim-once behaviour can be tested on a host against a local flag — the rest
/// of `open` touches a register and cannot run off-target.
fn claim(flag: &AtomicBool) -> Result<(), ScratchError> {
    flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .map(|_| ())
        .map_err(|_| ScratchError::InUse)
}

/// An owned handle to the scratch block.
pub struct Esp32Scratch {
    base: u32,
}

impl Esp32Scratch {
    /// Claim the scratch block, bring it up, and return the handle.
    ///
    /// The one public way to construct a live driver. Fails with
    /// [`ScratchError::InUse`] if the controller is already owned.
    pub fn open(port: &ScratchPort) -> hal::Result<Self> {
        claim(&CLAIMED)?;
        // A peripheral with pins routes them here, before first use, through
        // `Esp32PinMux::route(..)`. The scratch block has none.
        let this = Self { base: SCRATCH_BASE };
        // SAFETY: `base` is this controller's register block and `open` owns it
        // exclusively by the claim above.
        unsafe { reg::write(reg::at(this.base, SCRATCH_VALUE), port.seed) };
        Ok(this)
    }

    /// Wrap a base address without claiming the controller. **Self-tests only.**
    ///
    /// The self-test harness constructs drivers against loopback or scratch
    /// addresses outside the normal single-owner discipline. Application and
    /// board code call [`open`](Self::open) instead.
    ///
    /// # Safety
    /// `base` must be the register block of a scratch controller that no other
    /// handle is using.
    pub unsafe fn new(base: u32) -> Self {
        Self { base }
    }

    /// Write the scratch register. `&self`: ordinary traffic, like a bus's
    /// `PhysicalTransfer::exchange` — it needs no `&mut`, because the hardware,
    /// not this struct, holds the state.
    pub fn store(&self, value: u32) {
        // SAFETY: `base` is this handle's own register block.
        unsafe { reg::write(reg::at(self.base, SCRATCH_VALUE), value) };
    }

    /// Read the scratch register back. `&self`, for the same reason as
    /// [`store`](Self::store).
    pub fn load(&self) -> u32 {
        // SAFETY: as `store`.
        unsafe { reg::read(reg::at(self.base, SCRATCH_VALUE)) }
    }
}

impl Drop for Esp32Scratch {
    fn drop(&mut self) {
        // Release the claim so the controller can be opened again. A driver
        // that must leave the hardware running for the program's life would
        // simply not implement `Drop`.
        CLAIMED.store(false, Ordering::Release);
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_claim_wins_and_the_second_is_refused() {
        // A fresh local flag, so the test does not depend on the module static
        // or on other tests' ordering.
        let flag = AtomicBool::new(false);
        assert_eq!(claim(&flag), Ok(()), "first claim should take ownership");
        assert_eq!(
            claim(&flag),
            Err(ScratchError::InUse),
            "second claim must be refused"
        );
    }

    #[test]
    fn in_use_maps_to_a_named_hal_error() {
        let e: hal::Error = ScratchError::InUse.into();
        assert_eq!(e, hal::Error::Other("esp32 scratch already open"));
    }
}
