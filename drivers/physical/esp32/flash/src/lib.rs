// SPDX-License-Identifier: Apache-2.0

//! Raw SPI flash access, via the ROM's own driver.
//!
//! This is what `lib/kvstore` needs underneath it, and it is the one
//! peripheral on this chip you cannot drive casually: **the code is executing
//! out of the thing being written.**
//!
//! # Why every one of these functions is in IRAM
//!
//! Instructions come from flash through the cache. Programming the flash means
//! taking the SPI1 controller away from the cache, so for the duration of the
//! operation a cache miss has nowhere to go and the CPU stops — permanently,
//! with no fault. Everything on the path therefore has to already be in RAM
//! before the cache goes away: this crate's functions, anything they call, and
//! the ROM routines (which live in ROM, so they are fine).
//!
//! Hence `.iram1.flash` on every one of them — **and `#[inline(never)]`
//! beside it.** A `link_section` says where a function body goes; it says
//! nothing about a copy the optimiser folded into a caller. The first version
//! of this file had the attribute and no `inline(never)`, every function was
//! inlined into `.text`, and the board wedged the moment the cache went off.
//! The give-away was that the ELF contained no symbols for this crate at all.
//!
//! # The other core
//!
//! It is executing from flash too, and disabling the cache stops it just as
//! dead. **Nothing here stalls it.** Call these only while the second core is
//! parked — before `appcpu::start`, or after `appcpu::stop`. Doing otherwise
//! hangs that core silently, which looks exactly like a task that stopped
//! being scheduled.
//!
//! Stalling it properly belongs with whoever adds the first caller that needs
//! both, and wants `RTC_CNTL`'s stall rather than a reset.
//!
//! # ROM entry points
//!
//! Addresses from esp-idf `esp_rom/esp32/ld/esp32.rom.spiflash.ld` and
//! `esp32.rom.ld`. Hardcoded, matching how `soc_esp32::appcpu` reaches
//! `Cache_Flush`; this tree has no ROM linker script.
//!
//! | Routine | Address |
//! |---|---|
//! | `esp_rom_spiflash_erase_sector` | `0x4006_2CCC` |
//! | `esp_rom_spiflash_write` | `0x4006_2D50` |
//! | `esp_rom_spiflash_read` | `0x4006_2ED8` |
//!
//! # Why this does not work, and what would
//!
//! **The ROM's flash driver is not usable as-is.** Espressif's own linker
//! script says so, in a comment around two of the entry points:
//!
//! ```text
//! /* always using patched versions of these functions
//! PROVIDE ( esp_rom_spiflash_wait_idle = 0x400622c0 );
//! PROVIDE ( esp_rom_spiflash_unlock = 0x400????? );
//! */
//! ```
//!
//! Both are commented out, and `unlock`'s address is not even recorded —
//! `0x400?????`. `spi_flash/esp32/spi_flash_rom_patch.c` exists to replace
//! them. `esp_rom_spiflash_read` waits for the chip to go idle before it
//! returns, and a `wait_idle` that never completes is a read that never
//! returns, which is precisely the observed behaviour.
//!
//! That also explains why Zephyr's `flash_esp32.c` and Arduino both reach
//! flash through esp-idf's `esp_flash` layer rather than calling the ROM:
//! there is no supported way to call it directly.
//!
//! So the way forward is to stop using `ROM_READ`/`ROM_WRITE`/
//! `ROM_ERASE_SECTOR` and drive SPI1 here — send the read, page-program and
//! sector-erase commands, and poll the status register for the busy bit. That
//! is what the patched versions do, in about 150 lines, and it removes the
//! dependency on ROM behaviour nobody guarantees.
//!
//! # Ruled out, so nobody repeats it
//!
//! - **The cache.** Skipping the disable/restore entirely and calling the ROM
//!   read with the cache running hangs identically.
//! - **IRAM placement.** Verified against the ELF built with the self-test
//!   feature: every `with_cache_off` instantiation is in IRAM.
//! - **The chip description.** `apps/flashprobe` prints it off a running
//!   board: device `0x00C84016`, 4 MB, 64 KiB blocks, 4 KiB sectors, 256-byte
//!   pages. The ROM knows exactly what it is talking to.
//!
//! # What is still wrong
//!
//! `esp_rom_spiflash_read` does not return. Not a cache problem — the call
//! hangs with the cache left entirely alone, which was checked directly and
//! rules out the whole disable/restore path below. The cache sequence here is
//! still the one esp-idf uses and is worth keeping; it is simply not the bug.
//!
//! The remaining assumption, and now the prime suspect, is the line under
//! this one: that the bootloader's chip configuration survives into our image.
//! `g_rom_spiflash_chip` at `0x3FFAE270` holds the page, sector and block
//! sizes and the read command the ROM routine uses, and a routine polling for
//! a status that never comes is exactly what a wrong command looks like.
//! Zephyr and Arduino both avoid the question by going through esp-idf's
//! `esp_flash` layer, which initialises its own chip description rather than
//! inheriting one.
//!
//! The chip's parameters are assumed to have been configured by the
//! second-stage bootloader, so there is no `attach` or `config_param` call
//! here. That assumption is the next thing to test.

#![no_std]
// Same reason as `soc-esp32`: Xtensa inline asm is unstable, and this crate
// builds for the host too so its bounds checks can be tested there. An
// unconditional feature gate would be E0554 on stable and take those with it.
#![cfg_attr(target_arch = "xtensa", feature(asm_experimental_arch))]

use core::sync::atomic::Ordering;

// The SPI-NOR commands, because the ROM's driver cannot be called. See the
// module docs there.
#[path = "spi1.rs"]
mod spi1;

pub use spi1::{REG_SNAPSHOT, STATUS_TRACE};

/// Erase granularity, and the alignment every erase must respect.
pub const SECTOR_SIZE: u32 = 4096;

/// Why a flash operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashError {
    /// The chip did not finish in time.
    Timeout,
    /// The address or length is outside the region this handle owns.
    OutOfRange,
    /// The cache never reported itself idle, so it was left alone rather than
    /// disabled underneath the code that is running.
    CacheBusy,
    /// Address or length not a multiple of 4. The ROM routines take word
    /// pointers; a byte-aligned call reads or writes the wrong place rather
    /// than refusing.
    Misaligned,
}

// The ROM's Cache_Read_Disable/Enable are **not** used, and that is the fix.
// esp-idf replaces them, and says why in `spi_flash/cache_utils.c`:
//
//   > used to work around a bug where Cache_Read_Disable requires a call to
//   > Cache_Flush before Cache_Read_Enable, even if cached data was not
//   > modified.
//
// It drives DPORT directly instead, and waits for the cache to report itself
// idle before switching it off. Disabling a cache mid-operation is what wedged
// the first version of this file: the board went silent with no fault, which is
// what a CPU fetching from a cache that has gone away looks like.
const DPORT_BASE: u32 = 0x3FF0_0000;
/// `DPORT_PRO_CACHE_CTRL_REG`, with `PRO_CACHE_ENABLE` at bit 3.
const PRO_CACHE_CTRL: u32 = DPORT_BASE + 0x040;
const PRO_CACHE_ENABLE: u32 = 1 << 3;
/// `DPORT_PRO_CACHE_CTRL1_REG`. The mask bits say which windows the cache
/// serves; they are saved and restored around the operation rather than
/// assumed.
const PRO_CACHE_CTRL1: u32 = DPORT_BASE + 0x044;
const PRO_CACHE_MASK: u32 = 0x3F;
/// `DPORT_PRO_DCACHE_DBUG0_REG`, `PRO_CACHE_STATE` at [18:7]. A value of 1
/// means idle.
const PRO_DCACHE_DBUG0: u32 = DPORT_BASE + 0x3F0;
const PRO_CACHE_STATE_SHIFT: u32 = 7;
const PRO_CACHE_STATE_MASK: u32 = 0xFFF;
const PRO_CACHE_STATE_IDLE: u32 = 1;
/// Bound on the idle wait. The cache settles in a handful of cycles; this is
/// generous enough to absorb a burst and still fail rather than hang.
const CACHE_IDLE_SPINS: u32 = 100_000;

/// Diagnostic only.
const SKIP_CACHE: bool = false;


/// The last state seen when the wait timed out, with bit 31 set to mark it
/// written. Diagnostic: the whole question is what value this field actually
/// takes on this part, and it cannot be printed from inside the window.
pub static LAST_CACHE_STATE: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);


/// The ROM's description of the flash chip, at `0x3FFAE270`.
///
/// `esp_rom_spiflash_chip_t`: device id, chip size, block size, sector size,
/// page size, status mask. Every ROM routine reads its geometry and its read
/// command from here, so if it is not populated they poll for a status that
/// never arrives.
pub const ROM_SPIFLASH_CHIP: u32 = 0x3FFA_E270;

/// What the ROM thinks the flash is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChipInfo {
    pub device_id: u32,
    pub chip_size: u32,
    pub block_size: u32,
    pub sector_size: u32,
    pub page_size: u32,
    pub status_mask: u32,
}

impl ChipInfo {
    /// Read it back.
    ///
    /// # Safety
    /// Reads ROM-owned DRAM.
    pub unsafe fn read() -> Self {
        let p = ROM_SPIFLASH_CHIP as *const u32;
        Self {
            device_id: p.read_volatile(),
            chip_size: p.add(1).read_volatile(),
            block_size: p.add(2).read_volatile(),
            sector_size: p.add(3).read_volatile(),
            page_size: p.add(4).read_volatile(),
            status_mask: p.add(5).read_volatile(),
        }
    }

    /// Does this describe a real chip?
    ///
    /// The three geometry figures are fixed for every SPI NOR part this chip
    /// boots from. Anything else means nobody populated the struct, and the
    /// ROM routines are issuing commands built from rubbish.
    pub const fn looks_sane(&self) -> bool {
        self.page_size == 256 && self.sector_size == 4096 && self.chip_size >= 0x10_0000
    }
}

/// A region of flash, owned exclusively.
///
/// Offsets are relative to `base`, so a caller cannot reach past the partition
/// it was given by arithmetic alone.
pub struct FlashRegion {
    base: u32,
    len: u32,
}

impl FlashRegion {
    /// Take a region.
    ///
    /// # Safety
    /// `base` and `len` must describe a real partition that nothing else
    /// writes — the running image's own partition included. Erasing the wrong
    /// 4 KiB here is not recoverable at runtime.
    pub const unsafe fn new(base: u32, len: u32) -> Self {
        Self { base, len }
    }

    /// Bytes in the region.
    pub const fn len(&self) -> u32 {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn check(&self, offset: u32, len: u32) -> Result<u32, FlashError> {
        if offset % 4 != 0 || len % 4 != 0 {
            return Err(FlashError::Misaligned);
        }
        let end = offset.checked_add(len).ok_or(FlashError::OutOfRange)?;
        if end > self.len {
            return Err(FlashError::OutOfRange);
        }
        Ok(self.base + offset)
    }

    /// Read `buf.len()` bytes from `offset`.
    ///
    /// # Safety
    /// Runs with the cache disabled. See the module docs: the second core must
    /// not be running.
    #[inline(never)]
    #[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
    pub unsafe fn read(&self, offset: u32, buf: &mut [u32]) -> Result<(), FlashError> {
        let addr = self.check(offset, (buf.len() * 4) as u32)?;
        with_cache_off(|| spi1::read(addr, buf))
    }

    /// Write `data` at `offset`. The region must already be erased there.
    ///
    /// # Safety
    /// As [`FlashRegion::read`]. Writing over un-erased flash silently ANDs
    /// with what was there — flash can only clear bits — so the result is
    /// neither the old value nor the new one.
    #[inline(never)]
    #[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
    pub unsafe fn write(&self, offset: u32, data: &[u32]) -> Result<(), FlashError> {
        let addr = self.check(offset, (data.len() * 4) as u32)?;
        with_cache_off(|| spi1::write(addr, data))
    }

    /// Erase the sector containing `offset`.
    ///
    /// # Safety
    /// As [`FlashRegion::read`].
    #[inline(never)]
    #[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
    pub unsafe fn erase_sector(&self, offset: u32) -> Result<(), FlashError> {
        let addr = self.check(offset - (offset % SECTOR_SIZE), SECTOR_SIZE)?;
        with_cache_off(|| spi1::erase_sector(addr))
    }

    /// Erase the whole region.
    ///
    /// # Safety
    /// As [`FlashRegion::read`].
    #[inline(never)]
    #[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
    pub unsafe fn erase_all(&self) -> Result<(), FlashError> {
        let mut off = 0;
        while off < self.len {
            self.erase_sector(off)?;
            off += SECTOR_SIZE;
        }
        Ok(())
    }
}

/// Run `f` with the instruction cache disabled and interrupts masked.
///
/// The masking is not about atomicity. An interrupt handler is code, and code
/// is in flash unless it says otherwise — taking one here fetches through a
/// cache that is switched off.
///
/// # Safety
/// `f` and everything it calls must be in IRAM or ROM.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn with_cache_off(f: impl FnOnce() -> Result<(), FlashError>) -> Result<(), FlashError> {
    // Bisecting: run the transaction with the cache left alone, to split
    // "the cache dance is wrong" from "the transaction is wrong".
    if SKIP_CACHE {
        return f();
    }
    #[cfg(target_arch = "xtensa")]
    let saved: u32;
    #[cfg(target_arch = "xtensa")]
    core::arch::asm!("rsil {0}, 5", out(reg) saved);

    // Save which windows the cache serves, wait for it to go idle, then switch
    // it off. The wait is the part that cannot be skipped.
    let ctrl1 = PRO_CACHE_CTRL1 as *mut u32;
    let saved_mask = ctrl1.read_volatile() & PRO_CACHE_MASK;
    // Bounded, unlike esp-idf's, which spins forever. Everything else in this
    // tree bounds its polls, and an unbounded one here is indistinguishable
    // from the failure it is guarding against: interrupts are masked, so a
    // state that never reaches idle is a board that goes silent.
    let mut spins = 0u32;
    let mut last_state = 0u32;
    while spins < CACHE_IDLE_SPINS {
        last_state =
            ((PRO_DCACHE_DBUG0 as *const u32).read_volatile() >> PRO_CACHE_STATE_SHIFT)
                & PRO_CACHE_STATE_MASK;
        if last_state == PRO_CACHE_STATE_IDLE {
            break;
        }
        spins += 1;
    }
    if spins >= CACHE_IDLE_SPINS {
        LAST_CACHE_STATE.store(last_state | 0x8000_0000, Ordering::Relaxed);
        #[cfg(target_arch = "xtensa")]
        core::arch::asm!("wsr.ps {0}", "rsync", in(reg) saved);
        return Err(FlashError::CacheBusy);
    }
    let ctrl = PRO_CACHE_CTRL as *mut u32;
    ctrl.write_volatile(ctrl.read_volatile() & !PRO_CACHE_ENABLE);

    let r = f();

    ctrl.write_volatile(ctrl.read_volatile() | PRO_CACHE_ENABLE);
    ctrl1.write_volatile((ctrl1.read_volatile() & !PRO_CACHE_MASK) | saved_mask);

    #[cfg(target_arch = "xtensa")]
    core::arch::asm!("wsr.ps {0}", "rsync", in(reg) saved);

    r
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0x9000;
    const LEN: u32 = 0x6000;

    fn region() -> FlashRegion {
        unsafe { FlashRegion::new(BASE, LEN) }
    }

    #[test]
    fn rom_addresses_match_the_linker_scripts() {
        assert_eq!(PRO_CACHE_CTRL, 0x3FF0_0040);
        assert_eq!(PRO_CACHE_CTRL1, 0x3FF0_0044);
        assert_eq!(PRO_DCACHE_DBUG0, 0x3FF0_03F0);
        assert_eq!(PRO_CACHE_ENABLE, 1 << 3);
        assert_eq!(PRO_CACHE_STATE_SHIFT, 7);
        // The state field is [18:7], twelve bits. A mask that ran into bit 19
        // would take `PRO_WR_BAK_TO_READ` with it and the idle test would
        // never come true.
        assert_eq!(PRO_CACHE_STATE_MASK, 0xFFF);
    }

    #[test]
    fn an_offset_past_the_region_is_refused() {
        let r = region();
        assert_eq!(r.check(LEN, 4).unwrap_err(), FlashError::OutOfRange);
        assert_eq!(r.check(LEN - 4, 8).unwrap_err(), FlashError::OutOfRange);
        // The last word of the region is legal.
        assert_eq!(r.check(LEN - 4, 4).unwrap(), BASE + LEN - 4);
    }

    #[test]
    fn offsets_are_relative_to_the_region_not_the_chip() {
        // The whole point of the type. A caller that thinks in absolute
        // addresses would erase the bootloader.
        let r = region();
        assert_eq!(r.check(0, 4).unwrap(), BASE);
        assert_eq!(r.check(0x1000, 4).unwrap(), BASE + 0x1000);
    }

    #[test]
    fn misalignment_is_refused_rather_than_rounded() {
        // The ROM routines take word pointers. A byte-aligned call does not
        // fail -- it reads or writes somewhere else.
        let r = region();
        for off in [1u32, 2, 3] {
            assert_eq!(r.check(off, 4).unwrap_err(), FlashError::Misaligned);
        }
        for len in [1u32, 2, 3] {
            assert_eq!(r.check(0, len).unwrap_err(), FlashError::Misaligned);
        }
    }

    #[test]
    fn an_offset_that_would_overflow_is_refused() {
        let r = region();
        assert_eq!(r.check(u32::MAX - 3, 8).unwrap_err(), FlashError::OutOfRange);
    }

    #[test]
    fn the_sector_size_is_the_erase_granularity() {
        assert_eq!(SECTOR_SIZE, 4096);
        // The default nvs partition is exactly six sectors.
        assert_eq!(LEN % SECTOR_SIZE, 0);
        assert_eq!(LEN / SECTOR_SIZE, 6);
    }
}
