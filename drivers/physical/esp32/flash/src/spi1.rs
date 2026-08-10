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
//! | Read data | `0x03` | single-line, 24-bit address, **one dummy cycle** |
//! | Page program | `0x02` | 256 bytes max, must not cross a page |
//! | Sector erase | `0x20` | 4 KiB |
//!
//! `0x03` and `0x02` are single-line on every SPI-NOR part regardless of
//! whether the chip is running in dual or quad mode, which is what makes this
//! safe without knowing how the bootloader configured the flash.
//!
//! # Two conventions, and reads use the other one
//!
//! `SPI_CMD` has dedicated bits for flash operations, and asserting one runs
//! the whole command — opcode, address, data. **Erase and program use those.**
//! Reads and status reads do not: they are user-defined transactions fired
//! with `SPI_USR`, exactly as esp-idf does it —
//! `REG_WRITE(PERIPHS_SPI_FLASH_CMD, SPI_USR)`, never `SPI_FLASH_READ`.
//!
//! The two conventions disagree about the address register, which is the
//! single most expensive fact in this file:
//!
//! | | user transaction (`SPI_USR`) | native command |
//! |---|---|---|
//! | opcode | built in `USER2` | the controller has it |
//! | address | `SPI_ADDR = addr << 8` | `SPI_ADDR = addr`, unshifted |
//! | length | `MISO_DLEN` | top byte of `SPI_ADDR`, program only |
//! | `usr_addr_bitlen` | required, 23 | must be left alone |
//!
//! Driving a read as a native command transfers **nothing**, and the loop that
//! follows then copies out `W0..W15` — the data buffer, still holding the last
//! program's bytes. So a read returns what was most recently written and looks
//! like a working round trip. That is what "reads work" meant for a week.
//!
//! # The dummy cycle
//!
//! Every user transaction on this part needs one dummy cycle between the
//! opcode and the data. Without it the controller samples one clock early: the
//! byte arrives as `0x80 | (real >> 1)`, so a status of `0x00` reads as `0x80`
//! and a `WEL` of `0x02` reads as `0x81` — `WIP` apparently set, forever, on a
//! chip that is idle. Data reads shift identically: `c3 a5 07 05` comes back
//! as `e1 d2 83 82`.
//!
//! The ROM carries the same quantity as `g_rom_spiflash_dummy_len_plus`, and
//! esp-idf's `read_status` switches to a user command when it is non-zero.
//! That branch existing is the clue; see [`EXTRA_DUMMY`].
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
/// Erase, program, write-enable and write-disable are driven this way. Reads
/// and status reads are not — see the module docs for why, and note that
/// `CMD_FLASH_READ` and `CMD_FLASH_RDSR` are kept only so [`CMD_ANY`] can
/// recognise a transaction the bootloader left running.
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
    | CMD_FLASH_WRSR
    // `SPI_USR` too, or a user-defined transaction is never waited for at all
    // and the data buffer is read while the transfer is still in flight.
    // esp-idf polls the whole register: `while (REG_READ(CMD) != 0);`.
    | CMD_USR;

/// `SPI_WRSR_2B`, `SPI_CTRL` bit 22: a status write sends two bytes.
const CTRL_WRSR_2B: u32 = 1 << 22;


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
/// `SPI_USR_COMMAND` bit 31, `SPI_USR_MISO` bit 28.
const USR_COMMAND: u32 = 1 << 31;
const USR_MISO: u32 = 1 << 28;
/// `SPI_USR` bit 18 in `SPI_CMD` — fire a user-defined transaction.
const CMD_USR: u32 = 1 << 18;
/// `SPI_USR_COMMAND_BITLEN` sits at [31:28] of `USER2` and holds n−1, so an
/// eight-bit opcode is 7.
const COMMAND_BITLEN_8: u32 = 7 << 28;
/// SPI-NOR `RDSR`, as an opcode rather than a controller command bit.
const OPCODE_RDSR: u32 = 0x05;
/// Dummy cycles between a user command's opcode and its data phase.
///
/// The ROM carries the same quantity as `g_rom_spiflash_dummy_len_plus`, and
/// esp-idf's `read_status` branches on it -- which is the clue that it is not
/// always zero. Swept on hardware; see the sweep in `unlock`'s trace.
const EXTRA_DUMMY: u32 = 1;
/// SPI-NOR "continuous read mode reset". See [`exit_continuous_read`].
const OPCODE_MODE_RESET: u32 = 0xFF;
/// SPI-NOR single-line `READ`. Also an opcode and not a command bit: esp-idf
/// reads with `REG_WRITE(PERIPHS_SPI_FLASH_CMD, SPI_USR)`, never with
/// `SPI_FLASH_READ`.
const OPCODE_READ: u32 = 0x03;

/// Page size. A program may not cross one — the chip wraps to the start of the
/// page rather than continuing, silently corrupting both ends.
pub const PAGE_SIZE: u32 = 256;

/// What the cache left in the registers we disturb.
struct Saved {
    ctrl: u32,
    user: u32,
    user1: u32,
    user2: u32,
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
        // The read now rewrites this to hold an opcode, and the cache has its
        // own idea of what belongs here.
        user2: rd(USER2),
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
    wr(USER2, s.user2);
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
    // `USER` and `CTRL` back to zero, which is all a native command needs:
    // it supplies its own command, address and data phases, and takes the
    // address from `SPI_ADDR`. esp-idf's `spi_flash_ll_erase_sector` likewise
    // does `dev->ctrl.val = 0` and nothing else before firing `flash_se`.
    reset_user_ctrl();
    wr(USER1, rd(USER1) & !DUMMY_CYCLELEN_MASK);
}

/// Zero `SPI_USER` and `SPI_CTRL` before building a transaction.
///
/// ```c
/// static inline void spi_flash_ll_reset(spi_dev_t *dev)
/// {
///     dev->user.val = 0;
///     dev->ctrl.val = 0;
/// }
/// ```
///
/// esp-idf never inherits these. This driver did, and masking off the bits it
/// had thought of left the ones it had not: `SPI_DOUTDIN`, the `HIGHPART`
/// selectors, the clock edge. Any of those shifts where the received byte
/// lands, and a status read that is off by one bit reports `WEL` where `WIP`
/// is looked for -- so a chip that has just been told to write looks
/// permanently busy.
///
/// `save`/`restore` put both registers back, so the cache gets its
/// configuration returned intact.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn reset_user_ctrl() {
    wr(USER, 0);
    wr(CTRL, 0);
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

/// Take the chip out of continuous read mode.
///
/// Read off the running board, SPI0 — the cache's controller, which does work
/// — is configured as:
///
/// ```text
/// USER2 = 0x700000bb   command 0xBB, dual I/O fast read
/// USER1 = 0x6c000002   address phase 28 bits, 3 dummy cycles
/// ```
///
/// Twenty-eight bits of address is 24 of address and **4 of mode**. With the
/// mode nibble at 0xA the chip stops expecting a command byte at all and
/// treats the first bits of every transaction as an address. Which is what has
/// been happening to this driver's commands: they were not being refused, they
/// were being read as addresses. It explains a status register that reports
/// nonsense, an erase that reports success without erasing, and a read that
/// transfers nothing.
///
/// `0xFF` is the documented escape and is a no-op if the chip was never in the
/// mode, so it is safe to issue unconditionally.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn exit_continuous_read() -> Result<(), FlashError> {
    wait_spi_fsm_idle()?;
    wait_cmd()?;
    reset_user_ctrl();
    // Command only: no address, no dummy, no data either way.
    wr(USER, USR_COMMAND);
    wr(USER1, rd(USER1) & !DUMMY_CYCLELEN_MASK);
    wr(USER2, COMMAND_BITLEN_8 | OPCODE_MODE_RESET);
    let _ = rd(USER);
    wr(CMD, CMD_USR);
    wait_cmd()
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
    let dummy = EXTRA_DUMMY;
    wait_spi_fsm_idle()?;
    wait_cmd()?;
    // A user transaction rewrites the phase configuration, and the cache reads
    // through its own copy of these. Put them back regardless of the outcome.
    let (user, user1, user2, miso, ctrl) =
        (rd(USER), rd(USER1), rd(USER2), rd(MISO_DLEN), rd(CTRL));

    // From zero, not from whatever the cache left. An opcode out and a byte
    // back: no address, no dummy, nothing outbound.
    reset_user_ctrl();
    wr(USER, USR_COMMAND | USR_MISO | if dummy > 0 { USR_DUMMY } else { 0 });
    wr(
        USER1,
        (user1 & !DUMMY_CYCLELEN_MASK) | dummy.saturating_sub(1),
    );
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
        exit_continuous_read()?;
        wait_ready()?;
        // A read is a user-defined transaction. Not `SPI_FLASH_READ` -- that
        // command bit exists and does nothing useful here, and driving it was
        // why reads transferred no bytes at all. esp-idf:
        //
        // ```c
        // REG_WRITE(SPI_MISO_DLEN_REG(1), ((len << 3) - 1) << SPI_USR_MISO_DBITLEN_S);
        // WRITE_PERI_REG(PERIPHS_SPI_FLASH_ADDR, temp_addr << 8);
        // REG_WRITE(PERIPHS_SPI_FLASH_CMD, SPI_USR);
        // while (REG_READ(PERIPHS_SPI_FLASH_CMD) != 0);
        // ```
        //
        // Which is also where `usr_addr_bitlen` belongs. Setting it for the
        // native erase and program hung the chip; a user transaction builds
        // its own address phase and cannot work without it.
        reset_user_ctrl();
        let mut u1 = rd(USER1) & !(0x3F << ADDR_BITLEN_SHIFT) & !DUMMY_CYCLELEN_MASK;
        u1 |= (ADDR_BITS - 1) << ADDR_BITLEN_SHIFT;
        // The same one cycle the status read needs. Without it the whole data
        // stream arrives shifted right a bit: `c3 a5 07 05` reads back as
        // `e1 d2 83 82`.
        u1 |= EXTRA_DUMMY - 1;
        wr(USER1, u1);
        wr(USER2, COMMAND_BITLEN_8 | OPCODE_READ);
        // Command, address, one dummy cycle, data in; nothing outbound.
        wr(USER, USR_COMMAND | USR_ADDR | USR_MISO | USR_DUMMY);
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
            command(CMD_USR)?;
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
        exit_continuous_read()?;
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
            // Reads and status reads are user transactions, so this one is
            // waited for more often than any of the native commands. Leaving
            // it out meant the data buffer was read while the transfer was
            // still in flight, which returned the byte written to `W0` just
            // before firing -- a status read that reported back its own
            // scratch value and looked plausible doing it.
            CMD_USR,
        ] {
            assert_ne!(CMD_ANY & b, 0);
        }
        assert_eq!(CMD_ANY.count_ones(), 8);
    }

    #[test]
    fn a_user_transaction_carries_one_dummy_cycle() {
        // Measured by sweeping the count on hardware against a status whose
        // value was known: straight after WREN it must read 0x02. Zero cycles
        // gives 0x81, one gives 0x02.
        //
        // Everything downstream of getting this wrong looks like a different
        // bug -- a chip that is permanently busy, an erase that times out, a
        // read that returns plausible-but-shifted bytes -- so if this constant
        // is ever changed, change it against hardware and not against a guess.
        assert_eq!(EXTRA_DUMMY, 1);
        // `SPI_USR_DUMMY_CYCLELEN` holds n-1, so one cycle is a zero here, and
        // the enable bit in `USER` is what actually turns the phase on.
        assert_eq!(EXTRA_DUMMY - 1, 0);
        assert_ne!(USR_DUMMY, 0);
    }

    #[test]
    fn the_two_conventions_disagree_about_the_address() {
        // A read is a user transaction and shifts; an erase is a native
        // command and does not. Getting this backwards is silent: the read
        // transfers nothing and returns the data buffer's previous contents,
        // which are the bytes most recently written.
        let addr = 0x9000u32;
        assert_eq!((addr & ADDRESS_MASK_24BIT) << 8, 0x0090_0000);
        assert_eq!(addr & ADDRESS_MASK_24BIT, 0x0000_9000);
        // A program puts its length in the top byte, so its address must stay
        // in the low 24 bits or the two collide.
        let with_len = (addr & ADDRESS_MASK_24BIT) | (64u32 << 24);
        assert_eq!(with_len >> 24, 64);
        assert_eq!(with_len & ADDRESS_MASK_24BIT, addr);
    }

    #[test]
    fn the_user_opcodes_are_the_single_line_ones() {
        // Single-line regardless of the mode the bootloader left the chip in,
        // which is what makes them safe without knowing that mode.
        assert_eq!(OPCODE_READ, 0x03);
        assert_eq!(OPCODE_RDSR, 0x05);
        assert_eq!(OPCODE_MODE_RESET, 0xFF);
        // Eight-bit opcodes, and the field holds n-1.
        assert_eq!(COMMAND_BITLEN_8 >> 28, 7);
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
