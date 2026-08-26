// SPDX-License-Identifier: Apache-2.0

//! The `nvs` partition, as a key/value store.
//!
//! `lib/kvstore` has the format and recovery; the board selects and opens the
//! physical flash driver. This adapter joins them through `NorFlash`, without
//! making the portable format depend on a chip or a physical driver depend on
//! a storage format.
//!
//! # Alignment
//!
//! The flash trait takes word-aligned addresses and word counts. `kvstore` reads
//! at whatever offset an entry happens to start at, for whatever length a key
//! or a value happens to be. [`FlashStorage`] is where those two facts are
//! reconciled: reads are widened to the enclosing words and the result sliced
//! back out.
//!
//! Writes need no such treatment, and that is by construction rather than by
//! luck — `kvstore` aligns every entry to four bytes so the tail stays
//! word-aligned, which it does for the benefit of exactly this layer.

use kvstore::{Error as KvError, Storage};

#[cfg(target_os = "none")]
use hal::flash::NorFlash;

/// The board owns selection and construction; this adapter names no vendor.
#[cfg(target_os = "none")]
type SelectedFlash = board::NvsFlash;

/// Largest single transfer either direction. One `kvstore` entry is at most
/// 8 + 32 + 128 = 168 bytes; 256 leaves room without being generous.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
const SCRATCH_WORDS: usize = 64;

/// A `FlashRegion` that speaks [`Storage`].
pub struct FlashStorage {
    #[cfg(target_os = "none")]
    region: SelectedFlash,
}

impl FlashStorage {
    /// Take the `nvs` partition, its offset and length read from the active
    /// board manifest (`board::active::NVS_PARTITION`) rather than baked in —
    /// a board that flashes a custom partition table declares its own.
    ///
    /// # Safety
    /// Nothing else may access this partition. Obey `board::nvs_flash` and the
    /// selected driver's cache/XIP-off requirements, including debugger access.
    /// Panics if the partition cannot be opened (including duplicate RP2040 opens).
    pub unsafe fn nvs() -> Self {
        Self {
            #[cfg(target_os = "none")]
            region: board::nvs_flash().expect("NVS partition unavailable or already owned"),
        }
    }
}

#[cfg(target_os = "none")]
impl Storage for FlashStorage {
    const SECTOR_SIZE: u32 = <SelectedFlash as NorFlash>::SECTOR_SIZE;

    fn capacity(&self) -> u32 {
        NorFlash::len(&self.region)
    }

    fn read(&self, offset: u32, buf: &mut [u8]) -> Result<(), KvError> {
        if buf.is_empty() {
            return Ok(());
        }
        // Widen to whole words: the flash trait cannot start mid-word, and a
        // read that silently rounded the offset down would return the right
        // number of bytes from the wrong place.
        let start = offset & !3;
        let skip = (offset - start) as usize;
        let words = (skip + buf.len()).div_ceil(4);
        if words > SCRATCH_WORDS {
            return Err(KvError::Io);
        }
        let mut scratch = [0u32; SCRATCH_WORDS];
        unsafe { NorFlash::read(&self.region, start, &mut scratch[..words]) }.map_err(|_| KvError::Io)?;

        let bytes: &[u8] = unsafe {
            core::slice::from_raw_parts(scratch.as_ptr() as *const u8, words * 4)
        };
        buf.copy_from_slice(&bytes[skip..skip + buf.len()]);
        Ok(())
    }

    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), KvError> {
        if data.is_empty() {
            return Ok(());
        }
        // `kvstore` aligns every entry, so this should never fire. It is here
        // because "should never" is how the alignment guarantee gets quietly
        // dropped by a later change to the format.
        if offset % 4 != 0 || data.len() % 4 != 0 {
            return Err(KvError::Io);
        }
        let words = data.len() / 4;
        if words > SCRATCH_WORDS {
            return Err(KvError::Io);
        }
        // Through a word-aligned buffer: `data` is a byte slice and may not be
        // aligned, and the flash trait takes a `*const u32`.
        let mut scratch = [0u32; SCRATCH_WORDS];
        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                scratch.as_mut_ptr() as *mut u8,
                data.len(),
            );
            NorFlash::write(&self.region, offset, &scratch[..words])
        }
        .map_err(|_| KvError::Io)
    }

    fn erase_all(&mut self) -> Result<(), KvError> {
        unsafe { NorFlash::erase_all(&self.region) }.map_err(|_| KvError::Io)
    }
}

// A host build has no flash. The type still exists so callers compile, and
// every operation refuses rather than pretending to persist.
#[cfg(not(target_os = "none"))]
impl Storage for FlashStorage {
    const SECTOR_SIZE: u32 = 4096;
    fn capacity(&self) -> u32 {
        0
    }
    fn read(&self, _offset: u32, _buf: &mut [u8]) -> Result<(), KvError> {
        Err(KvError::Io)
    }
    fn write(&mut self, _offset: u32, _data: &[u8]) -> Result<(), KvError> {
        Err(KvError::Io)
    }
    fn erase_all(&mut self) -> Result<(), KvError> {
        Err(KvError::Io)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "soc-esp32")]
    #[test]
    fn the_partition_matches_what_the_bootloader_reports() {
        // The manifest quotes it from a running board's boot log. If espflash's
        // default table ever moves, this is the line that should fail first.
        let (offset, len) = crate::board::active::NVS_PARTITION;
        assert_eq!(offset, 0x9000);
        assert_eq!(len, 0x6000);
        // Whole sectors, or the last erase would run past the partition.
        assert_eq!(len % 4096, 0);
    }

    #[test]
    fn the_scratch_buffer_holds_a_whole_entry() {
        // 8-byte header + longest key + longest value. A scratch smaller than
        // one entry turns a legal `set` into an Io error at the largest sizes
        // only, which is the sort of thing that ships.
        let biggest = 8 + kvstore::MAX_KEY_LEN + kvstore::MAX_VALUE_LEN;
        assert!(
            SCRATCH_WORDS * 4 >= biggest,
            "scratch holds {} bytes, an entry needs {}",
            SCRATCH_WORDS * 4,
            biggest
        );
    }
}
