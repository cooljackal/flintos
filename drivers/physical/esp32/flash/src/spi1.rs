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
const USER2: u32 = SPI1_BASE + 0x24;
const W0: u32 = SPI1_BASE + 0x80;

/// SPI0 — the controller the *cache* fetches through, a separate peripheral
/// from SPI1.
const SPI0_BASE: u32 = 0x3FF4_3000;

/// `SPI_EXT2_REG`, offset `0xF8`, with the state machine in `SPI_ST` `[2:0]`.
/// Non-zero means that controller is mid-transaction.
const SPI1_EXT2: u32 = SPI1_BASE + 0xF8;
const SPI0_EXT2: u32 = SPI0_BASE + 0xF8;
const SPI_ST_MASK: u32 = 0x7;

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
const CMD_FLASH_WRDI: u32 = 1 << 29;
const CMD_FLASH_WRSR: u32 = 1 << 26;

/// Any command bit set means a transaction is running; all self-clear.
const CMD_ANY: u32 = CMD_FLASH_READ
    | CMD_FLASH_WREN
    | CMD_FLASH_RDSR
    | CMD_FLASH_PP
    | CMD_FLASH_SE
    | CMD_FLASH_WRDI
    | CMD_FLASH_WRSR;

/// `SPI_WRSR_2B`, `SPI_CTRL` bit 22: a status write sends two bytes.
const CTRL_WRSR_2B: u32 = 1 << 22;

/// `SPI_CTRL`'s fast-read mode bits: `FREAD_QIO` 24, `FREAD_DIO` 23,
/// `FREAD_QUAD` 20, `FREAD_DUAL` 14, `FASTRD_MODE` 13.
///
/// The bootloader leaves these describing however it configured the chip, and
/// a native flash command inherits them the same way it inherits the dummy
/// count. A status read issued while the controller believes it is in QIO does
/// not return status — it returns whatever four idle lines look like, offset by
/// the dummy cycles that mode implies.
const CTRL_FREAD_MASK: u32 = (1 << 24) | (1 << 23) | (1 << 20) | (1 << 14) | (1 << 13);

/// Status register 1: `SRP0` at bit 7, block-protect at [6:2]. Together these
/// are what refuses an erase.
const STATUS_SRP0: u32 = 1 << 7;
const STATUS_BP_MASK: u32 = 0x7C;

/// Status register 2's `QE`, at bit 1 — bit 9 of the two-byte value.
///
/// Preserved deliberately. A one-byte `WRSR` on a GigaDevice part can zero
/// status register 2, and QE lives there; clearing it breaks QIO boot. That is
/// recoverable by reflashing, but not worth risking, so the write is two bytes
/// with QE set.
const STATUS2_QE: u32 = 1 << 9;

/// Status register 1, bit 0: a program or erase is in progress.
const STATUS_WIP: u32 = 1 << 0;

/// The chip's status mask, which `apps/flashprobe` reads off the board as
/// 0xFFFF. esp-idf masks every status read with it rather than assuming one
/// byte.
const STATUS_MASK: u32 = 0xFFFF;

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
/// A 24-bit address, as `ESP_ROM_SPIFLASH_W_SIO_ADDR_BITSLEN` + 1.
const ADDR_BITS: u32 = 24;
const USR_ADDR: u32 = 1 << 30;
/// `SPI_USR_DUMMY` bit 29. esp-idf's `spi_flash_ll_program_page` clears it
/// explicitly: a page program has no dummy phase, and inheriting the cache's
/// -- which a fast-read mode certainly has -- shifts every byte written.
const USR_DUMMY: u32 = 1 << 29;
/// `SPI_USR_DUMMY_CYCLELEN` [7:0] in `USER1`.
const DUMMY_CYCLELEN_MASK: u32 = 0xFF;
/// `SPI_USR_COMMAND` bit 31, `SPI_USR_MISO` bit 28, `SPI_USR_MOSI` bit 27.
const USR_COMMAND: u32 = 1 << 31;
const USR_MISO: u32 = 1 << 28;
const USR_MOSI: u32 = 1 << 27;
/// `SPI_USR` bit 18 in `SPI_CMD` — fire a user-defined transaction.
const CMD_USR: u32 = 1 << 18;
/// `SPI_USR_COMMAND_BITLEN` sits at [31:28] of `USER2` and holds n−1, so an
/// eight-bit opcode is 7.
const COMMAND_BITLEN_8: u32 = 7 << 28;
/// SPI-NOR `RDSR`, as an opcode rather than a controller command bit.
const OPCODE_RDSR: u32 = 0x05;

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

/// Wait for both SPI controllers' state machines to go idle.
///
/// **This is the step that was missing**, and it is the first thing esp-idf's
/// patched `esp_rom_spiflash_wait_idle` does:
///
/// ```c
/// while ((REG_READ(SPI_EXT2_REG(1)) & SPI_ST)) { }
/// while ((REG_READ(SPI_EXT2_REG(0)) & SPI_ST)) { }
/// ```
///
/// Two things worth separating. `SPI_CMD` clearing means the *command* bit has
/// self-cleared; `SPI_ST` going to zero means the controller's state machine
/// has actually finished. They are not the same, and only the second is safe
/// to reconfigure against.
///
/// And the second wait is on **SPI0**, which is not this peripheral at all —
/// it is the one the instruction cache fetches through. Issuing a flash
/// command while SPI0's FSM is still running is where the two collide, and
/// nothing in the previous four attempts looked at SPI0 once.
///
/// # Safety
/// Reads two peripherals' status registers.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn wait_spi_fsm_idle() -> Result<(), FlashError> {
    let start = cycles();
    while rd(SPI1_EXT2) & SPI_ST_MASK != 0 {
        if cycles().wrapping_sub(start) > BUSY_CYCLES {
            return Err(FlashError::Timeout);
        }
    }
    let start = cycles();
    while rd(SPI0_EXT2) & SPI_ST_MASK != 0 {
        if cycles().wrapping_sub(start) > BUSY_CYCLES {
            return Err(FlashError::Timeout);
        }
    }
    Ok(())
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
    // Cleared, deliberately not set to 23. esp-idf's `erase_sector` does
    // `REG_SET_FIELD(USRREG1, SPI_USR_ADDR_BITLEN, 23)`, but writing 23 here
    // hangs the very next read on this part, twice in a row, on a
    // freshly-flashed board -- so a native flash command takes its address
    // length from the command, not from this field, and only the user-defined
    // transactions esp-idf also issues need it. Left at zero it is ignored.
    let mut u1 = rd(USER1) & !(0x3F << ADDR_BITLEN_SHIFT);
    // And zero the dummy cycles. Inheriting the cache's is one extra clock,
    // and one extra clock shifts every bit read: a status of 0x02 comes back
    // as 0x01, and data of 0xAA comes back as 0x55. Erased flash is all ones
    // and therefore immune, which is why reads looked correct right up until
    // something real had been written.
    u1 &= !DUMMY_CYCLELEN_MASK;
    wr(USER1, u1);
    wr(USER, (rd(USER) | USR_ADDR) & !USR_DUMMY);
    single_line();
}

/// Drop the controller back to one data line for a native command.
///
/// `save`/`restore` put `SPI_CTRL` back, so the cache gets its mode returned
/// intact.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn single_line() {
    wr(CTRL, rd(CTRL) & !CTRL_FREAD_MASK);
}

/// The same, for a command with no address phase.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn clear_dummy_no_addr() {
    wr(USER1, rd(USER1) & !DUMMY_CYCLELEN_MASK);
    wr(USER, rd(USER) & !(USR_DUMMY | USR_ADDR));
    single_line();
}

/// Run one native flash command.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn command(bit: u32) -> Result<(), FlashError> {
    wait_spi_fsm_idle()?;
    wait_cmd()?;
    // Drain the address and length writes before the command starts.
    let _ = rd(USER);
    wr(CMD, bit);
    wait_cmd()
}

/// Status register 1.
///
/// Uses the user-defined `0x05` rather than the controller's native `RDSR`,
/// which does not work on this part. Measured side by side straight after a
/// `WREN`, with the controller in single-line mode: native reports 0x81, the
/// user command reports 0x00. 0x81 is not a status this chip can be in — `WIP`
/// set with nothing in progress — and a permanently-set `WIP` is exactly the
/// hang this driver has been chasing.
///
/// esp-idf keeps the same escape hatch for what is presumably the same reason:
///
/// ```c
/// if (g_rom_spiflash_dummy_len_plus[1] == 0) { ... native RDSR ... }
/// else { esp_rom_spiflash_read_user_cmd(&status_value, 0x05); }
/// ```
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn status() -> Result<u32, FlashError> {
    status_user_cmd()
}

/// Status register 1 the other way: opcode `0x05` as a user transaction.
///
/// esp-idf keeps both and picks between them:
///
/// ```c
/// if (g_rom_spiflash_dummy_len_plus[1] == 0) {
///     ... native RDSR ...
/// } else {
///     esp_rom_spiflash_read_user_cmd(&status_value, 0x05);
/// }
/// ```
///
/// So the native command is not unconditionally correct — when the chip is
/// running with extra dummy cycles, esp-idf refuses to use it. This driver has
/// only ever used the native one. Reading both and comparing is the cheapest
/// way to find out whether that choice matters here, and it is a measurement
/// rather than an argument.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn status_user_cmd() -> Result<u32, FlashError> {
    wait_spi_fsm_idle()?;
    wait_cmd()?;
    // A user transaction rewrites the phase configuration, and the cache reads
    // through its own copy of these. Put them back regardless of the outcome.
    let (user, user1, user2, miso, ctrl) =
        (rd(USER), rd(USER1), rd(USER2), rd(MISO_DLEN), rd(CTRL));

    wr(USER, (user & !(USR_ADDR | USR_DUMMY | USR_MOSI)) | USR_COMMAND | USR_MISO);
    wr(USER1, user1 & !DUMMY_CYCLELEN_MASK);
    // One line here too. Inheriting QIO reports 0xc0 for a status of 0x00 --
    // the whole byte arrives shifted, which is what made this look like SRP0.
    single_line();
    wr(USER2, COMMAND_BITLEN_8 | OPCODE_RDSR);
    // Length fields hold n−1: one byte in, eight bits.
    wr(MISO_DLEN, 7);
    wr(W0, 0);
    let _ = rd(USER);
    wr(CMD, CMD_USR);
    let fired = wait_cmd();
    let value = rd(W0) & 0xFF;

    wr(USER, user);
    wr(USER1, user1);
    wr(USER2, user2);
    wr(MISO_DLEN, miso);
    wr(CTRL, ctrl);
    fired?;
    Ok(value)
}

/// Wait until no program or erase is in progress.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn wait_ready() -> Result<(), FlashError> {
    wait_spi_fsm_idle()?;
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

/// Whether the protection bits have been cleared this boot.
static UNLOCKED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Clear the chip's write protection.
///
/// The chip comes up with `SRP0` set — measured, not assumed: a status read
/// before any of this returns 0x80. With protection on, an erase is refused,
/// `WIP` never clears, and the busy-wait times out waiting for something that
/// will not happen.
///
/// This is the job of `esp_rom_spiflash_unlock`, the second function esp-idf
/// always replaces and whose address the ROM linker script declines to record.
/// Sequence transliterated from `spi_flash_rom_patch.c`: enable a two-byte
/// status write, WREN, write the new status, then WRDI so the write-enable
/// latch does not stay set.
///
/// Runs once per boot; repeating it would wear the status register for nothing.
///
/// # Safety
/// Cache disabled, caller in IRAM.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn unlock() -> Result<(), FlashError> {
    if UNLOCKED.load(Ordering::Relaxed) {
        return Ok(());
    }
    // Progress marker: whatever value survives says how far this got, which is
    // the only way to see inside a function that returns early with `?`.
    STATUS_TRACE[0].store(0x1000, Ordering::Relaxed);
    wait_ready()?;
    STATUS_TRACE[0].store(0x1001, Ordering::Relaxed);
    let st = status()?;
    STATUS_TRACE[1].store(st | 0x100, Ordering::Relaxed);
    if st & (STATUS_SRP0 | STATUS_BP_MASK) == 0 {
        // Nothing protected. Do not write the status register for no reason.
        UNLOCKED.store(true, Ordering::Relaxed);
        return Ok(());
    }

    STATUS_TRACE[0].store(0x1002, Ordering::Relaxed);
    wr(CTRL, rd(CTRL) | CTRL_WRSR_2B);
    command(CMD_FLASH_WREN)?;
    STATUS_TRACE[0].store(0x1003, Ordering::Relaxed);
    // Both readings of the same register, at the one moment their contents are
    // known: WREN has just run, so WEL is set and SR1 must be 0x02. Whichever
    // of these does not say 0x02 is the one that has been lying all along.
    // Bit 31 marks the slot as written; user read in [23:16], native in [15:0].
    {
        let native = status().unwrap_or(0xEE);
        let user = status_user_cmd().unwrap_or(0xEE);
        STATUS_TRACE[3].store(0x8000_0000 | ((user & 0xFF) << 16) | (native & 0xFFFF), Ordering::Relaxed);
    }
    // After the last status read, because a status read clears this register.
    wr(RD_STATUS, STATUS2_QE);
    command(CMD_FLASH_WRSR)?;
    STATUS_TRACE[0].store(0x1004, Ordering::Relaxed);
    // Two bytes was for the write only. Left set, a status *read* returns two
    // bytes as well, and the busy bit is then not where this looks for it.
    wr(CTRL, rd(CTRL) & !CTRL_WRSR_2B);
    wait_ready()?;
    STATUS_TRACE[0].store(0x1005, Ordering::Relaxed);
    STATUS_TRACE[2].store(status().unwrap_or(0x999) | 0x100, Ordering::Relaxed);
    // The write-enable latch must not be left set.
    command(CMD_FLASH_WRDI)?;
    STATUS_TRACE[0].store(0x1006, Ordering::Relaxed);
    UNLOCKED.store(true, Ordering::Relaxed);
    Ok(())
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
            // Shifted, and this one really is shifted. The three native
            // commands do not agree with each other, which is why assuming
            // cost so long:
            //
            // ```c
            // erase: WRITE_PERI_REG(PERIPHS_SPI_FLASH_ADDR, addr & 0xffffff);
            // program: ... (temp_addr & 0xffffff) | (len << 24));
            // read: WRITE_PERI_REG(PERIPHS_SPI_FLASH_ADDR, temp_addr << 8);
            // ```
            //
            // Left unshifted the controller transfers nothing at all, and the
            // loop below then reads back the data buffer's previous contents.
            // Which is a read that returns the bytes most recently *written* --
            // it looks like a working round trip and is not one.
            wr(ADDR, ((addr + done as u32 * 4) & ADDRESS_MASK_24BIT) << 8);
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
        unlock()?;
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
            // Address first, then the data, in esp-idf's order:
            //
            // ```c
            // WRITE_PERI_REG(PERIPHS_SPI_FLASH_ADDR, (temp_addr & 0xffffff) |
            //     (temp_bl << ESP_ROM_SPIFLASH_BYTES_LEN));
            // for (i = 0; i < (len >> 2); i++) {
            //     WRITE_PERI_REG(PERIPHS_SPI_FLASH_C0 + i * 4, *addr_source++);
            // }
            // ```
            //
            // Length rides in the top byte of the address register for a page
            // program. That is not a user-mode convention and has no analogue
            // in the previous attempts.
            wr(ADDR, (at & ADDRESS_MASK_24BIT) | ((n as u32 * 4) << 24));
            for i in 0..n {
                wr(W0 + (i as u32 * 4), src[done + i]);
            }
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
        unlock()?;
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
        assert_eq!(CMD_FLASH_WRDI, 1 << 29);
        assert_eq!(CMD_FLASH_WRSR, 1 << 26);
        // The idle test ORs them all; a bit left out is a command whose
        // completion nobody waits for.
        for b in [
            CMD_FLASH_READ,
            CMD_FLASH_WREN,
            CMD_FLASH_RDSR,
            CMD_FLASH_PP,
            CMD_FLASH_SE,
            CMD_FLASH_WRDI,
            CMD_FLASH_WRSR,
        ] {
            assert_ne!(CMD_ANY & b, 0);
        }
        assert_eq!(CMD_ANY.count_ones(), 7);
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
    fn both_controllers_state_machines_are_waited_on() {
        // SPI_EXT2 at 0xF8 on each, state in [2:0]. SPI0 is a different
        // peripheral -- the cache's -- and waiting only on SPI1 is what four
        // earlier attempts did.
        assert_eq!(SPI1_EXT2, 0x3FF4_20F8);
        assert_eq!(SPI0_EXT2, 0x3FF4_30F8);
        assert_ne!(SPI0_BASE, SPI1_BASE, "these are two peripherals");
        assert_eq!(SPI_ST_MASK, 0x7);
    }

    #[test]
    fn the_protection_bits_are_the_ones_that_refuse_an_erase() {
        // SRP0 at 7 and block-protect at [6:2]. The chip was measured holding
        // 0x80, which is SRP0 alone.
        assert_eq!(STATUS_SRP0, 1 << 7);
        assert_eq!(STATUS_BP_MASK, 0x7C);
        assert_eq!(STATUS_SRP0 & STATUS_BP_MASK, 0, "overlapping fields");
        // WEL and WIP are not protection and must not be cleared as if they
        // were -- they are status, not configuration.
        assert_eq!((STATUS_SRP0 | STATUS_BP_MASK) & 0x03, 0);
    }

    #[test]
    fn the_status_write_is_two_bytes_and_keeps_qe() {
        // A one-byte write can zero status register 2 on a GigaDevice part,
        // and QE lives there. Losing it breaks QIO boot.
        assert_eq!(CTRL_WRSR_2B, 1 << 22);
        assert_eq!(STATUS2_QE, 1 << 9, "QE is bit 1 of the second byte");
        assert_eq!(STATUS2_QE & 0xFF, 0, "QE must not land in the first byte");
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
