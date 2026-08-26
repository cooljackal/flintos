// SPDX-License-Identifier: Apache-2.0

//! Exclusive, word-oriented NOR region using the RP2040 ROM flash routines.
//!
//! Programming accepts erased words only; a page buffer contains ones outside
//! the requested words, so appending an entry does not reprogram older entries.
//! Both cores cooperate through the SoC XIP guard, all DMA must be idle, and
//! each ROM operation has a hardware watchdog deadline. A stalled flash resets
//! to recovery; it cannot safely return a software timeout with XIP unavailable.

#![no_std]

use hal::flash::NorFlash;

#[cfg(target_arch = "arm")]
mod hardware;

#[cfg(all(target_arch = "arm", feature = "flash-fault-injection"))]
pub use hardware::inject_xip_stall;

pub const SECTOR_SIZE: u32 = 4096;
pub const PAGE_SIZE: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Alignment,
    Range,
    InUse,
    NotErased,
    Verify,
    Unsupported,
    RomUnavailable,
    WatchdogTimebase,
    Exclusion(soc_rp2040::xip::Error),
}

fn validate_region(offset: u32, len: u32, flash_size: u32) -> Result<(), Error> {
    if offset % SECTOR_SIZE != 0 || len % SECTOR_SIZE != 0 {
        return Err(Error::Alignment);
    }
    if len == 0
        || flash_size > soc_rp2040::XIP_SIZE
        || offset >= flash_size
        || len > flash_size - offset
    {
        return Err(Error::Range);
    }
    Ok(())
}

fn validate_transfer(capacity: u32, offset: u32, words: usize) -> Result<(), Error> {
    if offset % 4 != 0 {
        return Err(Error::Alignment);
    }
    if offset > capacity || words > ((capacity - offset) / 4) as usize {
        return Err(Error::Range);
    }
    Ok(())
}

#[cfg(any(target_arch = "arm", test))]
fn stage_page(address: u32, data: &[u32], page: &mut [u32; PAGE_SIZE / 4]) -> (u32, usize) {
    let skip = (address as usize % PAGE_SIZE) / 4;
    let count = (PAGE_SIZE / 4 - skip).min(data.len());
    page.fill(u32::MAX);
    page[skip..skip + count].copy_from_slice(&data[..count]);
    (address & !(PAGE_SIZE as u32 - 1), count)
}

/// One board-reserved partition. No second handle (including overlapping
/// partitions) can open while this handle exists. Not `Sync`: calls may not race.
pub struct FlashRegion {
    offset: u32,
    len: u32,
    _not_sync: core::marker::PhantomData<core::cell::Cell<()>>,
}

impl FlashRegion {
    /// Open a board-reserved partition in a physical flash of `flash_size` bytes.
    ///
    /// # Safety
    /// The range must not contain firmware, boot data or anything accessed by
    /// another owner. `flash_size` must describe the fitted flash. Both cores
    /// must use the kernel's cooperating SIO IRQ. No NMI/debugger may access
    /// XIP during writes. Only task context outside critical sections may write.
    pub unsafe fn open(offset: u32, len: u32, flash_size: u32) -> Result<Self, Error> {
        validate_region(offset, len, flash_size)?;
        #[cfg(target_arch = "arm")]
        {
            if !hardware::claim() {
                return Err(Error::InUse);
            }
            Ok(Self {
                offset,
                len,
                _not_sync: core::marker::PhantomData,
            })
        }
        #[cfg(not(target_arch = "arm"))]
        Err(Error::Unsupported)
    }

    /// Erase one relative sector, leaving every other sector unchanged.
    ///
    /// # Safety
    /// The same XIP exclusion requirements as [`Self::open`] apply.
    pub unsafe fn erase_sector(&self, offset: u32) -> Result<(), Error> {
        if offset % SECTOR_SIZE != 0 {
            return Err(Error::Alignment);
        }
        validate_transfer(self.len, offset, SECTOR_SIZE as usize / 4)?;
        #[cfg(target_arch = "arm")]
        {
            hardware::operate(self.offset + offset, None)?;
            for word in 0..SECTOR_SIZE / 4 {
                if self.read_word(offset + word * 4) != u32::MAX {
                    return Err(Error::Verify);
                }
            }
            Ok(())
        }
        #[cfg(not(target_arch = "arm"))]
        Err(Error::Unsupported)
    }

    #[cfg(target_arch = "arm")]
    unsafe fn read_word(&self, offset: u32) -> u32 {
        ((soc_rp2040::XIP_BASE + self.offset + offset) as *const u32).read_volatile()
    }
}

impl Drop for FlashRegion {
    fn drop(&mut self) {
        #[cfg(target_arch = "arm")]
        unsafe {
            hardware::release()
        }
    }
}

impl NorFlash for FlashRegion {
    type Error = Error;
    const SECTOR_SIZE: u32 = SECTOR_SIZE;
    fn len(&self) -> u32 {
        self.len
    }

    unsafe fn read(&self, offset: u32, out: &mut [u32]) -> Result<(), Error> {
        validate_transfer(self.len, offset, out.len())?;
        #[cfg(target_arch = "arm")]
        {
            for (i, word) in out.iter_mut().enumerate() {
                *word = self.read_word(offset + i as u32 * 4);
            }
            Ok(())
        }
        #[cfg(not(target_arch = "arm"))]
        {
            let _ = self.offset;
            Err(Error::Unsupported)
        }
    }

    unsafe fn write(&self, offset: u32, data: &[u32]) -> Result<(), Error> {
        validate_transfer(self.len, offset, data.len())?;
        #[cfg(target_arch = "arm")]
        {
            // Validate the whole request before the first mutation.
            for i in 0..data.len() {
                if self.read_word(offset + i as u32 * 4) != u32::MAX {
                    return Err(Error::NotErased);
                }
            }
            let mut written = 0;
            while written < data.len() {
                let address = self.offset + offset + written as u32 * 4;
                let mut page = [u32::MAX; PAGE_SIZE / 4];
                let (page_address, count) = stage_page(address, &data[written..], &mut page);
                hardware::operate(page_address, Some(&page))?;
                written += count;
            }
            for (i, expected) in data.iter().enumerate() {
                if self.read_word(offset + i as u32 * 4) != *expected {
                    return Err(Error::Verify);
                }
            }
            Ok(())
        }
        #[cfg(not(target_arch = "arm"))]
        Err(Error::Unsupported)
    }

    unsafe fn erase_all(&self) -> Result<(), Error> {
        for offset in (0..self.len).step_by(SECTOR_SIZE as usize) {
            self.erase_sector(offset)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_word_offset_and_cross_page_length_preserves_unwritten_cells() {
        let mut source = [0u32; 130];
        for (i, word) in source.iter_mut().enumerate() {
            *word = i as u32;
        }
        for start_word in 0..64 {
            for length in 1..=source.len() {
                let start = 0x1fc000 + start_word * 4;
                let mut written = 0;
                while written < length {
                    let address = start + written as u32 * 4;
                    let mut page = [0; 64];
                    let (base, count) = stage_page(address, &source[written..length], &mut page);
                    let skip = ((address - base) / 4) as usize;
                    assert_eq!(base % 256, 0);
                    assert!(count > 0 && skip + count <= 64);
                    assert!(page[..skip].iter().all(|w| *w == u32::MAX));
                    assert_eq!(&page[skip..skip + count], &source[written..written + count]);
                    assert!(page[skip + count..].iter().all(|w| *w == u32::MAX));
                    written += count;
                }
                assert_eq!(written, length);
            }
        }
    }
    #[test]
    fn region_rejects_alignment_empty_overflow_and_aperture_mistakes() {
        assert_eq!(validate_region(0x1fc000, 0x4000, 0x200000), Ok(()));
        assert_eq!(validate_region(1, 4096, 0x200000), Err(Error::Alignment));
        assert_eq!(validate_region(4096, 4095, 0x200000), Err(Error::Alignment));
        assert_eq!(validate_region(4096, 0, 0x200000), Err(Error::Range));
        assert_eq!(
            validate_region(0xfffff000, 8192, 0x200000),
            Err(Error::Range)
        );
        assert_eq!(validate_region(0, 4096, 0x1001000), Err(Error::Range));
    }
    #[test]
    fn transfers_never_wrap_or_round_into_another_partition() {
        assert_eq!(validate_transfer(4096, 4092, 1), Ok(()));
        assert_eq!(validate_transfer(4096, 4096, 0), Ok(()));
        assert_eq!(validate_transfer(4096, 4092, 2), Err(Error::Range));
        assert_eq!(validate_transfer(4096, 4097, 0), Err(Error::Alignment));
        assert_eq!(validate_transfer(4096, 8192, 0), Err(Error::Range));
        assert_eq!(validate_transfer(4096, 0, usize::MAX), Err(Error::Range));
    }
    #[test]
    fn host_open_does_not_pretend_to_offer_persistent_storage() {
        assert!(matches!(
            unsafe { FlashRegion::open(0x1fc000, 0x4000, 0x200000) },
            Err(Error::Unsupported)
        ));
    }
}
