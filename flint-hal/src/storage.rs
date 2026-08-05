// SPDX-License-Identifier: Apache-2.0

//! Storage traits for block devices and raw byte-addressable storage.
//!
//! Block devices (flash, SD card, USB MSC) are accessed via `BlockDevice`.
//! Byte-addressable non-block storage (EEPROM, FRAM, battery-backed SRAM)
//! is accessed via `RawStorage`.

use core::fmt;

pub type StorageResult<T> = Result<T, StorageError>;

/// Storage operation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageError {
    /// I/O error from the underlying hardware.
    Io,
    /// Address or block number out of range.
    OutOfBounds,
    /// Storage is write-protected.
    WriteProtected,
    /// Operation not supported by this device.
    Unsupported,
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Block device trait (flash, SD card, USB MSC).
pub trait BlockDevice {
    /// Block size in bytes (e.g. 512 for SD, 4096 for NAND).
    const BLOCK_SIZE: usize;

    /// Read one block into `buf`.  `buf` must be at least `BLOCK_SIZE` bytes.
    fn read(&self, block: u32, buf: &mut [u8]) -> StorageResult<()>;

    /// Write one block from `buf`.
    fn write(&mut self, block: u32, buf: &[u8]) -> StorageResult<()>;

    /// Erase one block (sets all bytes to 0xFF or 0x00 depending on media).
    fn erase(&mut self, block: u32) -> StorageResult<()>;

    /// Total number of blocks on this device.
    fn block_count(&self) -> u32;
}

/// Byte-addressable non-block storage (EEPROM, FRAM, battery-backed SRAM).
///
/// Does NOT go through `BlockDevice` / LittleFS — provides direct
/// byte-level access.
pub trait RawStorage {
    /// Read `len` bytes starting at `addr`.
    fn read(&self, addr: u32, buf: &mut [u8]) -> StorageResult<()>;

    /// Write `buf` starting at `addr`.  No erase required.
    fn write(&mut self, addr: u32, data: &[u8]) -> StorageResult<()>;

    /// Total addressable bytes.
    fn capacity(&self) -> u32;

    /// Endurance hint in write cycles per byte (≈ 0 if unlimited).
    fn endurance_hint(&self) -> u32;
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn storage_error_display() {
        assert_eq!(std::format!("{}", StorageError::Io), "Io");
        assert_eq!(std::format!("{}", StorageError::OutOfBounds), "OutOfBounds");
    }

    #[test]
    fn storage_error_clone_copy_eq() {
        let a = StorageError::WriteProtected;
        let b = a;
        assert_eq!(a, b);
    }
}