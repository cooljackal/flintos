// SPDX-License-Identifier: Apache-2.0

//! SPI-NOR commands driven straight at SPI1. Included by [`crate`].
//!
//! This exists because the ROM's flash driver cannot be used. Espressif's own
//! linker script says so:
//!
//! ```text
//! /* always using patched versions of these functions
//! PROVIDE ( esp_rom_spiflash_wait_idle = 0x400622c0 );
//! PROVIDE ( esp_rom_spiflash_unlock = 0x400????? );
//! */
//! ```
//!
//! Commented out, with `unlock`'s address not even recorded. `esp_rom_spiflash_read`
//! waits for the chip before returning, and a broken wait is a read that never
//! returns — which is exactly what it did here, cache or no cache.
//!
//! So: four commands and a status poll, which is all a log-structured store
//! needs.
//!
//! | Command | Opcode | Notes |
//! |---|---|---|
//! | Read status register 1 | `0x05` | bit 0 is WIP, busy |
//! | Write enable | `0x06` | must precede every program and erase |
//! | Read data | `0x03` | single-line, 24-bit address, no dummy cycles |
//! | Page program | `0x02` | 256 bytes max, must not cross a page |
//! | Sector erase | `0x20` | 4 KiB |
//!
//! `0x03` and `0x02` are single-line on every SPI-NOR part regardless of
//! whether the chip is running in dual or quad mode, which is what makes this
//! safe without knowing how the bootloader configured the flash.
//!
//! # Where this differs from NuttX, which is the one to copy
//!
//! NuttX's `arch/xtensa/src/esp32/esp32_spiflash.c` is the only comparable
//! implementation that drives SPI1 itself — Zephyr and Arduino both hand off
//! to esp-idf. Two differences, and this code boot-loops the board while
//! NuttX does not:
//!
//! **1. It keeps the controller's read mode; this one destroys it.**
//! `esp32_set_read_opt` reads `SPI_CTRL`, works out which mode the cache is
//! using — QIO, DIO, dual, quad, plain — and issues the *matching* opcode with
//! that mode's address length and dummy cycles: `0xEB` for QIO, `0xBB` for
//! DIO, `0x6B`, `0x3B`, and so on. This module clears the mode bits and uses
//! plain `0x03` instead, on the theory that a single-line opcode always works.
//! It may, but the clearing does not survive: `SPI_CTRL` is restored, and the
//! cache still comes back unable to fetch.
//!
//! **2. It configures the transaction shape once, not per call.**
//! `esp32_readonce` writes only `MISO_DLEN`, `ADDR`, `RD_STATUS` and then
//! fires `SPI_CMD`. `SPI_USER`, `USER1` and `USER2` were set up earlier and
//! are left alone. This module rewrites all three on every transaction,
//! including the status reads, so the registers the cache depends on are
//! churned far more often — and `SPI_RD_STATUS` is never cleared at all.
//!
//! It also waits differently: `while (SPI_CMD != 0)`, the whole register,
//! rather than testing the `USR` bit.
//!
//! Following NuttX rather than inventing this is the way forward.
//!
//! # Every function here is in IRAM, and that is not optional
//!
//! The cache is disabled while these run, so a call into flash is the last
//! instruction the CPU executes. `#[link_section]` alone does not achieve
//! that — it says where a body goes and nothing about a copy the optimiser
//! folded elsewhere — so it is paired with `#[inline(never)]` on every
//! function, including the private helpers.
//!
//! This was got wrong twice. The first time every function was inlined into
//! `.text` and the attribute did nothing. The second time the wrapper was in
//! IRAM and everything it called was not, which reads identically from the
//! source and is visible only in the ELF:
//!
//! ```text
//! IRAM   with_cache_off::<...read::{closure#0}>
//! FLASH  spi1::read
//! ```
//!
//! Check placement against the built image, not the attributes.
//!
//! # SPI1 belongs to the cache
//!
//! It is the controller the instruction cache fetches through. Every register
//! touched here is saved and restored, because leaving `SPI_CTRL`'s
//! fast-read-mode bits or `SPI_USER`'s phase selection changed means the cache
//! resumes with the wrong transaction shape and the next instruction fetch
//! returns nonsense. The cache must already be disabled before any of this is
//! called — see [`crate::with_cache_off`].

use core::sync::atomic::Ordering;

use crate::{FlashError, LAST_CACHE_STATE};

/// SPI1, the flash controller.
const SPI1_BASE: u32 = 0x3FF4_2000;

const CMD: u32 = SPI1_BASE + 0x00;
const ADDR: u32 = SPI1_BASE + 0x04;
const CTRL: u32 = SPI1_BASE + 0x08;
const USER: u32 = SPI1_BASE + 0x1C;
const MOSI_DLEN: u32 = SPI1_BASE + 0x28;
const MISO_DLEN: u32 = SPI1_BASE + 0x2C;
const RD_STATUS: u32 = SPI1_BASE + 0x10;
const USER1: u32 = SPI1_BASE + 0x20;
const W0: u32 = SPI1_BASE + 0x80;

/// **SPI1 knows the SPI-NOR protocol natively.**
///
/// This is the thing three previous attempts missed. `SPI_CMD` has dedicated
/// bits for flash operations — read, write-enable, read-status, page-program,
/// sector-erase — and asserting one runs the whole command: opcode, address
/// phase, data phase, all of it. There is no command to compose, no address
/// bit-length to set, no dummy cycles to get right.
///
/// esp-idf drives it this way (`hal/esp32/spi_flash_ll.h`):
///
/// ```c
/// static inline void spi_flash_ll_erase_sector(spi_dev_t *dev) {
///     dev->ctrl.val = 0;
///     dev->cmd.flash_se = 1;
/// }
/// ```
///
/// Two conventions therefore exist for driving SPI1, and mixing them is what
/// wedged the controller:
///
/// | | user-defined (`SPI_USR`) | native flash command |
/// |---|---|---|
/// | opcode | you build it in `USER2` | the controller has it |
/// | address | `SPI_ADDR = addr << 8` | `SPI_ADDR = addr`, **unshifted** |
/// | length | `MISO_DLEN`/`MOSI_DLEN` | top byte of `SPI_ADDR` for program |
/// | phases | `USER`/`USER1` | not yours |
///
/// Previous attempts used the user-defined route with a shifted address, and
/// attempt B then borrowed the cache's register setup — which describes the
/// *other* convention. Hence a controller SPI0 could not use afterwards.
const CMD_FLASH_READ: u32 = 1 << 31;
const CMD_FLASH_WREN: u32 = 1 << 30;
const CMD_FLASH_RDSR: u32 = 1 << 27;
const CMD_FLASH_PP: u32 = 1 << 25;
const CMD_FLASH_SE: u32 = 1 << 24;

/// Any command bit set means a transaction is running; all self-clear.
const CMD_ANY: u32 = CMD_FLASH_READ | CMD_FLASH_WREN | CMD_FLASH_RDSR | CMD_FLASH_PP | CMD_FLASH_SE;

/// Status register 1, bit 0: a program or erase is in progress.
const STATUS_WIP: u32 = 1 << 0;

/// Bound on a command's completion. A transaction is microseconds.
const XFER_SPINS: u32 = 200_000;
/// Bound on the busy poll, **in CPU cycles rather than iterations**.
///
/// An iteration count is the wrong unit here: each turn of that loop is a whole
/// status transaction, so fifty million of them is minutes, and a failure that
/// takes minutes is indistinguishable from a hang. It was read as one.
///
/// Two seconds at 80 MHz. A sector erase is tens of milliseconds and a page
/// program well under one, so this only fires for a chip that has genuinely
/// stopped answering.
const BUSY_CYCLES: u32 = 160_000_000;

/// A native flash command's address field is 24 bits.
const ADDRESS_MASK_24BIT: u32 = 0x00FF_FFFF;

/// `SPI_USR_ADDR_BITLEN` [31:26] in `USER1`, and `SPI_USR_ADDR` bit 30 in
/// `USER`.
const ADDR_BITLEN_SHIFT: u32 = 26;
const USR_ADDR: u32 = 1 << 30;
/// `SPI_USR_DUMMY` bit 29. esp-idf's `spi_flash_ll_program_page` clears it
/// explicitly: a page program has no dummy phase, and inheriting the cache's
/// -- which a fast-read mode certainly has -- shifts every byte written.
const USR_DUMMY: u32 = 1 << 29;
/// `SPI_USR_DUMMY_CYCLELEN` [7:0] in `USER1`.
const DUMMY_CYCLELEN_MASK: u32 = 0xFF;

/// Page size. A program may not cross one — the chip wraps to the start of the
/// page rather than continuing, silently corrupting both ends.
pub const PAGE_SIZE: u32 = 256;

/// What the cache left in the registers we disturb.
struct Saved {
    ctrl: u32,
    user: u32,
    user1: u32,
    addr: u32,
    miso_dlen: u32,
    mosi_dlen: u32,
}

/// The cycle counter, for bounding a wait in time.
///
/// `inline(always)`: this is called from inside the cache-off window, and a
/// real call would go to flash.
#[inline(always)]
unsafe fn cycles() -> u32 {
    #[cfg(target_arch = "xtensa")]
    {
        let c: u32;
        core::arch::asm!("rsr.ccount {0}", out(reg) c);
        c
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        0
    }
}

#[inline(always)]
unsafe fn rd(reg: u32) -> u32 {
    (reg as *const u32).read_volatile()
}

#[inline(always)]
unsafe fn wr(reg: u32, v: u32) {
    (reg as *mut u32).write_volatile(v);
}

#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn save() -> Saved {
    Saved {
        ctrl: rd(CTRL),
        user: rd(USER),
        user1: rd(USER1),
        addr: rd(ADDR),
        miso_dlen: rd(MISO_DLEN),
        mosi_dlen: rd(MOSI_DLEN),
    }
}

#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn restore(s: &Saved) {
    wr(CTRL, s.ctrl);
    wr(USER, s.user);
    wr(USER1, s.user1);
    wr(ADDR, s.addr);
    wr(MISO_DLEN, s.miso_dlen);
    wr(MOSI_DLEN, s.mosi_dlen);
    // Drain before the caller re-enables the cache.
    let _ = rd(USER);
}

/// Snapshots of SPI1 taken at the top of the first two reads, so a read that
/// works and a read that returns 0x55 can be compared. Diagnostic; printing
/// cannot happen inside the cache-off window.
pub static REG_SNAPSHOT: [core::sync::atomic::AtomicU32; 16] =
    [const { core::sync::atomic::AtomicU32::new(0) }; 16];
static SNAP_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Capture the eight registers that matter, into slot 0 then slot 1.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn snapshot() {
    let which = SNAP_COUNT.fetch_add(1, Ordering::Relaxed);
    if which > 1 {
        return;
    }
    // Unrolled deliberately. An array literal is constant data, and constant
    // data lives in `.rodata` -- which is flash. Reading it with the cache
    // disabled is fatal in exactly the same way calling into flash is, and it
    // is easier to miss because it does not look like a function call.
    let base = (which as usize) * 8;
    REG_SNAPSHOT[base].store(rd(CMD), Ordering::Relaxed);
    REG_SNAPSHOT[base + 1].store(rd(ADDR), Ordering::Relaxed);
    REG_SNAPSHOT[base + 2].store(rd(CTRL), Ordering::Relaxed);
    REG_SNAPSHOT[base + 3].store(rd(USER), Ordering::Relaxed);
    REG_SNAPSHOT[base + 4].store(rd(USER1), Ordering::Relaxed);
    REG_SNAPSHOT[base + 5].store(rd(MISO_DLEN), Ordering::Relaxed);
    REG_SNAPSHOT[base + 6].store(rd(MOSI_DLEN), Ordering::Relaxed);
    REG_SNAPSHOT[base + 7].store(rd(RD_STATUS), Ordering::Relaxed);
}

/// Status register readings taken through one page program, so the step that
/// changes the chip's state can be identified rather than guessed at.
///
/// Slots: 0 before WREN, 1 after WREN, 2 after the program command, 3 once the
/// chip reports ready. Bit 7 set (`SRP0`) is what we are hunting.
pub static STATUS_TRACE: [core::sync::atomic::AtomicU32; 4] =
    [const { core::sync::atomic::AtomicU32::new(0) }; 4];
static TRACED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Record status into `slot`, for the first program only.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn trace(slot: usize, first: bool) {
    if !first {
        return;
    }
    // 0x100 marks the slot as written, so an untouched slot is distinguishable
    // from a genuine status of zero.
    match status() {
        Ok(v) => STATUS_TRACE[slot].store(v | 0x100, Ordering::Relaxed),
        Err(_) => STATUS_TRACE[slot].store(0x200, Ordering::Relaxed),
    }
}

/// Wait for whatever command is running to self-clear.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn wait_cmd() -> Result<(), FlashError> {
    let mut spins = 0;
    while rd(CMD) & CMD_ANY != 0 {
        spins += 1;
        if spins > XFER_SPINS {
            return Err(FlashError::Timeout);
        }
    }
    Ok(())
}

/// Force a 24-bit address phase.
///
/// **Erase and program need this and reads do not**, which is why reads worked
/// while everything else corrupted the chip. esp-idf calls
/// `spi_flash_ll_set_addr_bitlen(dev, 24)` before every erase and program:
///
/// ```c
/// dev->user1.usr_addr_bitlen = (bitlen - 1);
/// dev->user.usr_addr = bitlen ? 1 : 0;
/// ```
///
/// Left alone, these inherit whatever the cache is using. If the cache runs
/// QIO its address phase is 32 bits — 24 of address and 8 of mode — so an
/// erase issued under that configuration lands on the wrong sector. Which
/// erases part of the application image, and is why that failure survived a
/// reset while the read failure did not.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn set_addr_bitlen_24() {
    let mut u1 = rd(USER1) & !(0x3F << ADDR_BITLEN_SHIFT);
    // And zero the dummy cycles. Inheriting the cache's is one extra clock,
    // and one extra clock shifts every bit read: a status of 0x02 comes back
    // as 0x01, and data of 0xAA comes back as 0x55. Erased flash is all ones
    // and therefore immune, which is why reads looked correct right up until
    // something real had been written.
    u1 &= !DUMMY_CYCLELEN_MASK;
    wr(USER1, u1);
    wr(USER, (rd(USER) | USR_ADDR) & !USR_DUMMY);
}

/// The same, for a command with no address phase.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn clear_dummy_no_addr() {
    wr(USER1, rd(USER1) & !DUMMY_CYCLELEN_MASK);
    wr(USER, rd(USER) & !(USR_DUMMY | USR_ADDR));
}

/// Run one native flash command.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn command(bit: u32) -> Result<(), FlashError> {
    wait_cmd()?;
    // Drain the address and length writes before the command starts.
    let _ = rd(USER);
    wr(CMD, bit);
    wait_cmd()
}

/// Status register 1 via the controller's own read-status command.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn status() -> Result<u32, FlashError> {
    // A status read has no address and no dummy phase either.
    clear_dummy_no_addr();
    // Clear the result register before asking, as NuttX does. Left alone it
    // holds whatever the last read put there, and the controller only writes
    // the bits it received -- so a stale high bit survives and every reading
    // looks shifted.
    wr(RD_STATUS, 0);
    command(CMD_FLASH_RDSR)?;
    Ok(rd(RD_STATUS) & 0xFF)
}

/// Wait until no program or erase is in progress.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn wait_ready() -> Result<(), FlashError> {
    let start = cycles();
    loop {
        if status()? & STATUS_WIP == 0 {
            return Ok(());
        }
        // Wrapping: CCOUNT is 32 bits and rolls over every 54 seconds at
        // 80 MHz, which is well inside the window this bounds.
        if cycles().wrapping_sub(start) > BUSY_CYCLES {
            LAST_CACHE_STATE.store(0xDEAD_0001, Ordering::Relaxed);
            return Err(FlashError::Timeout);
        }
    }
}

/// Read `dest.len()` words from `addr`.
///
/// # Safety
/// The cache must be disabled and the caller in IRAM.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
pub unsafe fn read(addr: u32, dest: &mut [u32]) -> Result<(), FlashError> {
    snapshot();
    let saved = save();
    let r = (|| {
        wait_ready()?;
        // Explicit, not inherited. A read after a program was returning 0x55
        // for every byte -- the program leaves the address phase and dummy
        // configuration changed, and a read that borrows whatever is there
        // samples at the wrong alignment.
        set_addr_bitlen_24();
        wr(USER, rd(USER) & !USR_DUMMY);
        let mut done = 0usize;
        while done < dest.len() {
            let n = (dest.len() - done).min(16);
            // Unshifted: the native command's address register is not the
            // user-mode one.
            wr(ADDR, (addr + done as u32 * 4) & ADDRESS_MASK_24BIT);
            wr(MISO_DLEN, (n as u32 * 4) * 8 - 1);
            command(CMD_FLASH_READ)?;
            for i in 0..n {
                dest[done + i] = rd(W0 + (i as u32 * 4));
            }
            done += n;
        }
        Ok(())
    })();
    restore(&saved);
    r
}

/// Program `src` at `addr`. The region must be erased.
///
/// # Safety
/// As [`read`].
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
pub unsafe fn write(addr: u32, src: &[u32]) -> Result<(), FlashError> {
    let saved = save();
    let r = (|| {
        let mut done = 0usize;
        while done < src.len() {
            let at = addr + done as u32 * 4;
            let to_page_end = (PAGE_SIZE - (at % PAGE_SIZE)) / 4;
            let n = (src.len() - done).min(16).min(to_page_end as usize);
            wait_ready()?;
            command(CMD_FLASH_WREN)?;
            set_addr_bitlen_24();
            // No dummy phase on a program. See USR_DUMMY.
            wr(USER, rd(USER) & !USR_DUMMY);
            for i in 0..n {
                wr(W0 + (i as u32 * 4), src[done + i]);
            }
            // Length rides in the top byte of the address register for a page
            // program. That is not a user-mode convention and has no analogue
            // in the previous attempts.
            wr(ADDR, (at & ADDRESS_MASK_24BIT) | ((n as u32 * 4) << 24));
            command(CMD_FLASH_PP)?;
            done += n;
        }
        wait_ready()
    })();
    restore(&saved);
    r
}

/// Erase the 4 KiB sector containing `addr`.
///
/// # Safety
/// As [`read`].
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
pub unsafe fn erase_sector(addr: u32) -> Result<(), FlashError> {
    let saved = save();
    let r = (|| {
        let first = TRACED.fetch_add(1, Ordering::Relaxed) == 0;
        wait_ready()?;
        trace(0, first);
        command(CMD_FLASH_WREN)?;
        trace(1, first);
        set_addr_bitlen_24();
        // esp-idf clears CTRL entirely before a sector erase.
        wr(CTRL, 0);
        wr(ADDR, (addr & !(4096 - 1)) & ADDRESS_MASK_24BIT);
        command(CMD_FLASH_SE)?;
        trace(2, first);
        let r = wait_ready();
        trace(3, first);
        r
    })();
    restore(&saved);
    r
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_offsets_match_spi_reg_h() {
        assert_eq!(SPI1_BASE, 0x3FF4_2000);
        assert_eq!(CMD, SPI1_BASE);
        assert_eq!(ADDR, SPI1_BASE + 0x04);
        assert_eq!(CTRL, SPI1_BASE + 0x08);
        assert_eq!(USER, SPI1_BASE + 0x1C);
        assert_eq!(RD_STATUS, SPI1_BASE + 0x10);
        assert_eq!(W0, SPI1_BASE + 0x80);
    }

    #[test]
    fn a_native_command_takes_the_address_unshifted() {
        // The opposite of the user-defined convention, where SPI_ADDR holds
        // the address at [31:8]. Shifting it here sends the wrong sector --
        // and mixing the two conventions is what wedged three earlier
        // attempts.
        assert_eq!(0x9000u32 & ADDRESS_MASK_24BIT, 0x9000);
        assert_eq!(ADDRESS_MASK_24BIT, 0x00FF_FFFF);
    }

    #[test]
    fn a_twenty_four_bit_address_phase_encodes_as_twenty_three() {
        // Biased by one, like every length on this peripheral, and it sits at
        // bit 26. Inheriting the cache's value instead -- 32 bits in QIO, for
        // the mode bits -- puts an erase on the wrong sector.
        assert_eq!((24 - 1) << ADDR_BITLEN_SHIFT, 23 << 26);
        assert_eq!(USR_ADDR, 1 << 30);
    }

    #[test]
    fn a_program_has_no_dummy_phase() {
        // Inheriting the cache's dummy cycles shifts every byte written, so
        // the data lands but reads back wrong -- which is a far quieter
        // failure than not writing at all.
        assert_eq!(USR_DUMMY, 1 << 29);
        assert_eq!(USR_DUMMY & USR_ADDR, 0);
    }

    #[test]
    fn a_page_program_carries_its_length_in_the_top_byte() {
        // esp-idf: (address & ADDRESS_MASK_24BIT) | (length << 24).
        let packed = (0x9000u32 & ADDRESS_MASK_24BIT) | (64u32 << 24);
        assert_eq!(packed & ADDRESS_MASK_24BIT, 0x9000);
        assert_eq!(packed >> 24, 64);
    }

    #[test]
    fn the_flash_command_bits_are_distinct_and_all_covered() {
        assert_eq!(CMD_FLASH_READ, 1 << 31);
        assert_eq!(CMD_FLASH_WREN, 1 << 30);
        assert_eq!(CMD_FLASH_RDSR, 1 << 27);
        assert_eq!(CMD_FLASH_PP, 1 << 25);
        assert_eq!(CMD_FLASH_SE, 1 << 24);
        // The idle test ORs them all; a bit left out is a command whose
        // completion nobody waits for.
        for b in [CMD_FLASH_READ, CMD_FLASH_WREN, CMD_FLASH_RDSR, CMD_FLASH_PP, CMD_FLASH_SE] {
            assert_ne!(CMD_ANY & b, 0);
        }
        assert_eq!(CMD_ANY.count_ones(), 5);
    }

    #[test]
    fn a_program_stops_at_the_page_boundary() {
        // The property the chip enforces by wrapping rather than erroring.
        for (at, want) in [(0u32, 64u32), (0xF0, 4), (0x100, 64), (0x1FC, 1)] {
            let to_page_end = (PAGE_SIZE - (at % PAGE_SIZE)) / 4;
            assert_eq!(to_page_end, want, "at {at:#x}");
        }
    }

    #[test]
    fn the_busy_wait_is_bounded_in_time_not_iterations() {
        // Each iteration is a whole status transaction, so an iteration count
        // says nothing about how long the bound actually is. Fifty million of
        // them was minutes, and a failure taking minutes reads as a hang --
        // which is exactly how it was read.
        assert_eq!(BUSY_CYCLES, 160_000_000, "two seconds at 80 MHz");
        // Comfortably longer than a sector erase, comfortably shorter than a
        // CCOUNT rollover at 80 MHz, which is about 54 seconds.
        assert!(BUSY_CYCLES > 80_000_000 / 2, "shorter than a sector erase");
        assert!(BUSY_CYCLES < u32::MAX / 2, "risks a rollover ambiguity");
        assert!(XFER_SPINS > 0);
    }
}
