// SPDX-License-Identifier: Apache-2.0

//! A word-addressed NOR-flash region, for the key/value store to sit on.
//!
//! The kernel's `nvs` module is the joint between `kvstore` (the format) and a
//! SoC's flash driver (the registers). It drives the flash through this trait
//! so the joint's read/write/erase logic names no chip: a SoC's driver
//! implements [`NorFlash`], and the board selects one concrete type with the
//! build. The word-oriented API (`&[u32]`, not `&[u8]`) is deliberate — the
//! ESP32's SPI1 controller takes word pointers and counts, and pretending
//! otherwise at this layer only hides the alignment the caller must respect.

/// A bounded region of NOR flash, addressed in whole 32-bit words within it.
///
/// Every method is `unsafe`: on some SoCs (the ESP32) these run with the
/// instruction cache off, which stops any core still fetching from flash — see
/// the driver for the second-core contract.
pub trait NorFlash {
    /// The error a failed operation returns. The caller usually flattens it —
    /// `kvstore` maps any flash error to one I/O error — so it is an associated
    /// type rather than a shared enum every driver must map onto.
    type Error;

    /// Erase granularity, in bytes. An `erase` clears a whole sector.
    const SECTOR_SIZE: u32;

    /// The region's length in bytes.
    fn len(&self) -> u32;

    /// Whether the region is empty (length zero).
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read `out.len()` words starting at byte `offset` (word-aligned).
    ///
    /// # Safety
    /// See the trait note: may run with the cache off.
    unsafe fn read(&self, offset: u32, out: &mut [u32]) -> Result<(), Self::Error>;

    /// Write `data` starting at byte `offset` (word-aligned).
    ///
    /// # Safety
    /// See the trait note.
    unsafe fn write(&self, offset: u32, data: &[u32]) -> Result<(), Self::Error>;

    /// Erase the whole region.
    ///
    /// # Safety
    /// See the trait note.
    unsafe fn erase_all(&self) -> Result<(), Self::Error>;
}
