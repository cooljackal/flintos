// SPDX-License-Identifier: Apache-2.0

//! Raw SPI flash access, driving SPI1 directly.
//!
//! This is what `lib/kvstore` needs underneath it, and it is the one
//! peripheral on this chip you cannot drive casually: **the code is executing
//! out of the thing being written.**
//!
//! # Not through the ROM
//!
//! The obvious route is the ROM's own flash driver, and it is a dead end.
//! Espressif's linker script comments out `esp_rom_spiflash_wait_idle` and
//! `esp_rom_spiflash_unlock` with the note "always using patched versions of
//! these functions" -- `unlock`'s address is not even recorded, written
//! `0x400?????` -- and `spi_flash/esp32/spi_flash_rom_patch.c` exists to
//! replace them. `esp_rom_spiflash_read` waits for the chip to go idle before
//! returning, so a broken `wait_idle` is a read that never returns, which is
//! exactly what this driver did for a week. Zephyr's `flash_esp32.c` and
//! Arduino both reach flash through esp-idf's `esp_flash` layer rather than
//! calling the ROM, for the same reason: there is no supported way to call it
//! directly.
//!
//! So [`spi1`] sends the commands itself. See its module docs for the part
//! that is genuinely surprising -- this chip needs two different transaction
//! conventions, and using the wrong one for a read is silent rather than
//! loud.
//!
//! # Why every one of these functions is in IRAM
//!
//! Instructions come from flash through the cache. Programming the flash means
//! taking the SPI1 controller away from the cache, so for the duration of the
//! operation a cache miss has nowhere to go and the CPU stops — permanently,
//! with no fault. Everything on the path therefore has to already be in RAM
//! before the cache goes away: this crate's functions and anything they call,
//! constant data included — an array literal lives in `.rodata`, which is
//! flash, and reading it here is fatal in exactly the way calling into flash
//! is, while looking nothing like a call.
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
//! dead. So it has to be got out of the way, and **it is asked rather than
//! frozen** — see the note above [`park_this_core`]. Core 0 raises
//! `FROM_CPU_2`, core 1's handler turns off its own cache, says it is parked
//! and spins until released.
//!
//! This used to be a hardware stall, and the argument for that being safe was
//! that nothing ran between the stall and the release, so core 1 could be
//! frozen holding a lock and nobody on this side would ask for it. #69 made
//! IRAM-safe interrupts run in exactly that window, which retired the
//! argument. esp-idf
//! (`spi_flash_disable_interrupts_caches_and_other_cpu`) and NuttX
//! (`esp32_spiflash_opstart`) both ask, and neither stalls.
//!
//! The stall survives as a **bounded fallback**: if core 1 does not answer
//! within `PARK_SPINS`, the old path runs and [`PARK_FELL_BACK`] records it.
//! A board whose park signal was never routed then still writes flash, badly
//! rather than not at all.

#![no_std]
// Same reason as `soc-esp32`: Xtensa inline asm is unstable, and this crate
// builds for the host too so its bounds checks can be tested there. An
// unconditional feature gate would be E0554 on stable and take those with it.
#![cfg_attr(target_arch = "xtensa", feature(asm_experimental_arch))]

use core::sync::atomic::Ordering;

// From the SoC crate rather than redeclared here. This file used to carry its
// own `DPORT_BASE = 0x3FF0_0000`, and `spi1.rs` its own `SPI1_BASE`, both of
// which `soc_esp32::addr` already had.
use soc_esp32::addr::DPORT_BASE;

// The SPI-NOR commands, because the ROM's driver cannot be called. See the
// module docs above.
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
    /// Address or length not a multiple of 4. The SPI1 driver takes word
    /// pointers; a byte-aligned call reads or writes the wrong place rather
    /// than refusing.
    Misaligned,
    /// The flash chip's JEDEC manufacturer id is not one whose status-register
    /// layout this driver knows, and the chip arrived with its block-protect
    /// bits set — so unlocking it would mean writing a status register blind.
    ///
    /// Refusing loses the ability to write flash on that board. Guessing can
    /// lose the ability to boot it: see [`spi1::unlock`].
    UnknownChip,
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
/// `DPORT_PRO_CACHE_CTRL_REG`, with `PRO_CACHE_ENABLE` at bit 3.
const PRO_CACHE_CTRL: u32 = DPORT_BASE + 0x040;
/// `DPORT_APP_CACHE_CTRL_REG` and `..._CTRL1_REG`, the second core's pair.
/// Same bit layout as the PRO ones; confirmed against esp-idf's `dport_reg.h`.
///
/// Read only on the target — a host build has no second core to stall — but
/// the tests pin the addresses either way, since a wrong one here disables
/// nothing and corrupts whatever core 1 was executing.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
const APP_CACHE_CTRL: u32 = DPORT_BASE + 0x058;
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
const APP_CACHE_CTRL1: u32 = DPORT_BASE + 0x05C;
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

/// Why a cache-off window last failed, or 0 if none has.
///
/// Nothing inside the window can print: the cache is off or about to be, and
/// the log path is in flash. So the two failures that are otherwise invisible
/// leave a value here for a caller to read afterwards -- `0x8000_0000 | state`
/// when the idle wait timed out, `0xDEAD_0001` when the chip never reported
/// ready. `apps/flashprobe` prints it.
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

/// Run `f` with the instruction cache disabled and unsafe interrupts masked.
///
/// The masking is not about atomicity. An interrupt handler is code, and code
/// is in flash unless it says otherwise — taking one here fetches through a
/// cache that is switched off, and the core stops.
///
/// # Selective, not blanket
///
/// This used to raise `PS.INTLEVEL` to 5, masking everything. Safe, and a
/// real-time defect: a sector erase is tens of milliseconds, and for all of it
/// the tick stopped and no driver interrupt was serviced. Long enough to drop
/// a Wi-Fi link, and poor behaviour even without one.
///
/// Now only the interrupts that have *not* promised to be IRAM-safe are
/// masked, through `INTENABLE`. A handler registered with
/// `interrupt::register_iram_safe` keeps running throughout. esp-idf
/// (`esp_intr_noniram_disable`) and NuttX (`esp32_spiflash_opstart`) both do
/// exactly this, and for the same reason.
///
/// `PS.INTLEVEL` is still raised, but only across the two short windows where
/// `INTENABLE` and the cache registers are being changed — a handful of
/// instructions rather than the whole operation.
///
/// # Safety
/// `f` and everything it calls must be in IRAM or ROM. So must any handler
/// registered as IRAM-safe, which is a promise this cannot check.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
unsafe fn with_cache_off(f: impl FnOnce() -> Result<(), FlashError>) -> Result<(), FlashError> {
    // Briefly, to make the INTENABLE read-modify-write atomic against an
    // interrupt arriving between the read and the write.
    #[cfg(target_arch = "xtensa")]
    let saved: u32;
    #[cfg(target_arch = "xtensa")]
    core::arch::asm!("rsil {0}, 5", out(reg) saved);
    #[cfg(target_os = "none")]
    let saved_intenable = kernel_interrupt_mask();

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
        // Every exit from here on owes an `INTENABLE` restore. This one is the
        // easiest to forget, because it is the path that does no work: leaving
        // without it strands the mask at the IRAM-safe set -- today the empty
        // set -- so the tick never fires again and the board goes quiet with
        // nothing pointing back here. Any new early return needs this too.
        #[cfg(target_os = "none")]
        kernel_interrupt_restore(saved_intenable);
        #[cfg(target_arch = "xtensa")]
        core::arch::asm!("wsr.ps {0}", "rsync", in(reg) saved);
        return Err(FlashError::CacheBusy);
    }
    // Stall the second core before its cache goes away. Order matters: a core
    // still fetching when the cache dies stops mid-instruction, and nothing
    // reports it.
    // `is_running`, not `!is_stalled`. A core that was never started is held
    // in reset with its clock gated and its stall bits clear, so the negation
    // called it running -- and every flash read on a single-core application
    // then paid `stall` and `unstall`, which each wait 32768 cycles for the
    // RTC's slow-clock domain. 819 µs of settling around a read that takes
    // microseconds, on every read.
    #[cfg(target_os = "none")]
    let app_was_running = soc_esp32::appcpu::is_running();

    // Ask first, stall only if asking fails. See the long note above
    // `park_this_core`: a parked core provably holds no lock, a stalled one
    // may hold any of them, and IRAM-safe interrupts now run in this window.
    #[cfg(target_os = "none")]
    let app_parked = app_was_running && park_other_core();

    #[cfg(target_os = "none")]
    let app_stalled = app_was_running && !app_parked;

    // The fallback path, unchanged from when it was the only path: freeze the
    // core and reach across to its cache registers.
    #[cfg(target_os = "none")]
    let app_saved_mask = if app_stalled {
        soc_esp32::appcpu::stall();
        let c1 = APP_CACHE_CTRL1 as *mut u32;
        let saved = c1.read_volatile() & PRO_CACHE_MASK;
        let c = APP_CACHE_CTRL as *mut u32;
        c.write_volatile(c.read_volatile() & !PRO_CACHE_ENABLE);
        saved
    } else {
        0
    };

    let ctrl = PRO_CACHE_CTRL as *mut u32;
    ctrl.write_volatile(ctrl.read_volatile() & !PRO_CACHE_ENABLE);

    // Back to the caller's interrupt level for the long part. Everything
    // unsafe to run now is masked in INTENABLE; anything still enabled said it
    // could cope with the cache being off.
    #[cfg(target_arch = "xtensa")]
    core::arch::asm!("wsr.ps {0}", "rsync", in(reg) saved);

    let r = f();

    // And masked again for the restore, which is another read-modify-write.
    #[cfg(target_arch = "xtensa")]
    core::arch::asm!("rsil {0}, 5", out(reg) _);

    ctrl.write_volatile(ctrl.read_volatile() | PRO_CACHE_ENABLE);
    ctrl1.write_volatile((ctrl1.read_volatile() & !PRO_CACHE_MASK) | saved_mask);

    // And the second core, in the reverse order: cache back first, then let it
    // run. Released before its cache is restored, it would fetch through a
    // disabled one.
    #[cfg(target_os = "none")]
    if app_stalled {
        let c = APP_CACHE_CTRL as *mut u32;
        c.write_volatile(c.read_volatile() | PRO_CACHE_ENABLE);
        let c1 = APP_CACHE_CTRL1 as *mut u32;
        c1.write_volatile((c1.read_volatile() & !PRO_CACHE_MASK) | app_saved_mask);
        soc_esp32::appcpu::unstall_now();
    }

    // A parked core restores its own cache, so there is nothing to reach
    // across for -- only the release, and the wait for it to be out.
    #[cfg(target_os = "none")]
    if app_parked {
        release_other_core();
    }

    #[cfg(target_os = "none")]
    kernel_interrupt_restore(saved_intenable);

    #[cfg(target_arch = "xtensa")]
    core::arch::asm!("wsr.ps {0}", "rsync", in(reg) saved);

    r
}

// ── The interrupt hook ──────────────────────────────────────────────────────
//
// This driver is Layer 1 and may not name `kernel` — `make check-layers`
// enforces it, and rightly: a physical driver reaching into the scheduler is
// how a driver stops being portable. But deciding *which* interrupts are safe
// to leave enabled means knowing which handlers promised to be IRAM-safe, and
// that register lives in the kernel.
//
// So the kernel installs a pair of functions at startup and this calls them if
// they are there. Two atomics and a setter; the indirection buys the layer
// boundary and costs one indirect call per flash operation, which against tens
// of milliseconds of erase is nothing.
//
// Uninstalled, `mask` returns `u32::MAX` — the sentinel for "no hook" — and
// the operation runs with only the `rsil 5` it always had. That is the old
// behaviour exactly, which is what makes installing the hook a change that can
// be reverted by not calling the setter.

use core::sync::atomic::AtomicUsize;

static MASK_HOOK: AtomicUsize = AtomicUsize::new(0);
static RESTORE_HOOK: AtomicUsize = AtomicUsize::new(0);

/// The chip's JEDEC id, straight from the chip: manufacturer in bits [7:0],
/// memory type in [15:8], capacity in [23:16].
///
/// This is what [`spi1::unlock`] gates on, so it is the number to look at when
/// a board reports `UnknownChip` — and the reason it exists separately from
/// [`ChipInfo::read`], which returns the *ROM's cached* view in a different
/// byte order (`(mfr << 16) | (type << 8) | capacity`). Two representations of
/// one fact is exactly the sort of thing to read off the hardware rather than
/// derive.
///
/// # Safety
/// Runs a flash transaction with the cache disabled, so the same rules as any
/// other operation in this crate apply.
pub unsafe fn jedec_id() -> Result<u32, FlashError> {
    let mut out = 0u32;
    unsafe { with_cache_off(|| spi1::jedec_id().map(|v| out = v)) }?;
    Ok(out)
}

// ── Parking the other core ──────────────────────────────────────────────────
//
// The hardware stall this replaces was safe by an argument that stopped being
// true. `appcpu::stall()` freezes core 1 wherever it happens to be, and a
// frozen core does not release what it holds -- so if it were stalled holding
// a `Spinlock`, anything on core 0 that wanted that lock would wait forever.
// What made it safe was that *nothing ran* between the stall and the release.
// Then #69 made IRAM-safe interrupts run there, and the argument went with it.
//
// esp-idf and NuttX both solve it the same way, and neither stalls:
//
//     esp-idf   spi_flash_disable_interrupts_caches_and_other_cpu, cache_utils.c
//               -> esp_ipc_call(other_cpu, spi_flash_op_block_func)
//     NuttX     esp32_spiflash_opstart, esp32_spiflash.c
//               -> nxsem_post(&g_disable_non_iram_isr_on_core[other_cpu])
//
// In both, the other core is *asked*: it runs code of its own, disables its
// own cache, says it is ready, and spins until released. It is never frozen
// mid-instruction.
//
// **Why that is enough, in FlintOS's terms.** `Spinlock::with` takes the lock
// inside `arch::cs_with`, so a lock is only ever held with interrupts masked
// on the holding core. An interrupt therefore cannot land on core 1 while
// core 1 holds a lock -- so a core parked *by an interrupt* provably holds
// none. That is the same invariant esp-idf relies on, arrived at from the same
// direction, and it is what makes the handshake correct rather than merely
// more polite than a stall.
//
// The signal is `FROM_CPU_2`, routed only in the APP crossbar table, which is
// esp-idf's choice of channel for exactly this job.

/// Set by core 1 once it has parked: its cache is off and it is spinning.
#[cfg(target_os = "none")]
static PARKED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Set by core 0 to ask core 1 to park, cleared to release it.
#[cfg(target_os = "none")]
static PARK_REQUESTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// How long core 0 waits for core 1 to park before giving up on the handshake.
///
/// Bounded, and the fallback is the old hardware stall. Core 1 answers within
/// an interrupt latency when it is healthy; if it does not, something is wrong
/// with the routing or the handler, and hanging here would be a board that
/// goes silent on its first flash write with nothing pointing at the cause.
/// Stalling instead is worse but recoverable, and it says so in the log.
#[cfg(target_os = "none")]
const PARK_SPINS: u32 = 2_000_000;

/// True if the last flash operation had to fall back to stalling core 1.
///
/// Diagnostics, and the only way the fallback is visible: it cannot log from
/// where it happens.
pub static PARK_FELL_BACK: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// How many times core 1 has been parked by handshake rather than stalled.
///
/// Exists so the handshake can be shown to have *run*. Without it a silent
/// fallback and a working handshake look identical from outside, and code that
/// is never executed is the failure mode this project keeps rediscovering.
pub static PARKS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// The handler core 1 runs when core 0 asks it to park. **IRAM throughout.**
///
/// Register it on the second core with `interrupt::connect_iram_safe` against
/// `crosscore::Signal::FromCpu2`, routed in the APP table. The kernel does
/// that in `join_scheduler`; this crate cannot, being Layer 1.
///
/// Acknowledging comes first and matters: the from-CPU signal is
/// level-triggered, so a handler that spins without clearing it re-enters the
/// moment it returns.
///
/// # Safety
/// Trap context on the APP CPU only. Disables that core's cache, so everything
/// from here until it is restored must be in IRAM — which is why this does so
/// little.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
pub unsafe fn park_this_core() {
    #[cfg(target_os = "none")]
    {
        use core::sync::atomic::Ordering;

        soc_esp32::crosscore::clear(soc_esp32::crosscore::Signal::FromCpu2);

        // This core's own cache, saved and switched off by this core. Core 0
        // used to reach across and do it; doing it here is what lets core 1
        // choose the moment.
        let c1 = APP_CACHE_CTRL1 as *mut u32;
        let saved_mask = c1.read_volatile() & PRO_CACHE_MASK;
        let c = APP_CACHE_CTRL as *mut u32;
        c.write_volatile(c.read_volatile() & !PRO_CACHE_ENABLE);

        PARKED.store(true, Ordering::SeqCst);
        while PARK_REQUESTED.load(Ordering::SeqCst) {
            core::hint::spin_loop();
        }

        c.write_volatile(c.read_volatile() | PRO_CACHE_ENABLE);
        c1.write_volatile((c1.read_volatile() & !PRO_CACHE_MASK) | saved_mask);
        PARKED.store(false, Ordering::SeqCst);
    }
}

/// Ask core 1 to park. `true` if it did; `false` if the caller must stall it.
///
/// # Safety
/// Raises a DPORT signal. Must be followed by [`release_other_core`].
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
#[cfg(target_os = "none")]
unsafe fn park_other_core() -> bool {
    use core::sync::atomic::Ordering;

    PARKED.store(false, Ordering::SeqCst);
    PARK_REQUESTED.store(true, Ordering::SeqCst);
    soc_esp32::crosscore::raise(soc_esp32::crosscore::Signal::FromCpu2);

    let mut spins = 0u32;
    while !PARKED.load(Ordering::SeqCst) {
        if spins >= PARK_SPINS {
            // Give up cleanly: withdraw the request so a late handler does not
            // park against a release that has already happened.
            PARK_REQUESTED.store(false, Ordering::SeqCst);
            soc_esp32::crosscore::clear(soc_esp32::crosscore::Signal::FromCpu2);
            PARK_FELL_BACK.store(true, Ordering::Relaxed);
            return false;
        }
        spins += 1;
        core::hint::spin_loop();
    }
    PARKS.fetch_add(1, Ordering::Relaxed);
    true
}

/// Let core 1 go, and wait for it to have its cache back.
///
/// The wait is not optional. Returning while core 1 is still spinning with its
/// cache off leaves it fetching nothing the moment it resumes, and the failure
/// appears on the *other* core.
///
/// # Safety
/// Must match a [`park_other_core`] that returned `true`.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.flash")]
#[cfg(target_os = "none")]
unsafe fn release_other_core() {
    use core::sync::atomic::Ordering;
    PARK_REQUESTED.store(false, Ordering::SeqCst);
    while PARKED.load(Ordering::SeqCst) {
        core::hint::spin_loop();
    }
}

/// Tell the driver how to mask interrupts that cannot survive the cache going
/// away, and how to put them back.
///
/// Call once, from the kernel, before any flash operation. Without it flash
/// still works and simply masks everything for the duration.
///
/// # Safety
/// `mask` must return a value `restore` accepts, and neither may touch flash
/// — they are called with the cache on, but the second is called immediately
/// before it comes back, so keeping both in IRAM is the safe habit.
pub unsafe fn set_interrupt_hooks(mask: unsafe fn() -> u32, restore: unsafe fn(u32)) {
    MASK_HOOK.store(mask as usize, Ordering::Relaxed);
    RESTORE_HOOK.store(restore as usize, Ordering::Relaxed);
}

/// Mask what cannot run with the cache off. `u32::MAX` means no hook.
#[cfg(target_os = "none")]
#[inline(always)]
unsafe fn kernel_interrupt_mask() -> u32 {
    let f = MASK_HOOK.load(Ordering::Relaxed);
    if f == 0 {
        return u32::MAX;
    }
    unsafe { core::mem::transmute::<usize, unsafe fn() -> u32>(f)() }
}

/// Put back what [`kernel_interrupt_mask`] returned.
#[cfg(target_os = "none")]
#[inline(always)]
unsafe fn kernel_interrupt_restore(saved: u32) {
    let f = RESTORE_HOOK.load(Ordering::Relaxed);
    if f == 0 {
        return;
    }
    unsafe { core::mem::transmute::<usize, unsafe fn(u32)>(f)(saved) }
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
    fn the_app_core_cache_registers_are_where_dport_says() {
        // From esp-idf's dport_reg.h, which is also where the PRO pair below
        // came from. A wrong address here disables nothing and the second core
        // keeps fetching through a cache the flash write is about to
        // invalidate -- which corrupts whatever it was executing, arbitrarily
        // far from this line.
        assert_eq!(APP_CACHE_CTRL, 0x3FF0_0058);
        assert_eq!(APP_CACHE_CTRL1, 0x3FF0_005C);
        // Same layout as the PRO registers, which is why they share the
        // enable bit and window mask.
        assert_eq!(APP_CACHE_CTRL - PRO_CACHE_CTRL, 0x18);
        assert_eq!(APP_CACHE_CTRL1 - APP_CACHE_CTRL, 0x4);
        assert_eq!(PRO_CACHE_CTRL1 - PRO_CACHE_CTRL, 0x4);
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
        // The SPI1 driver takes word pointers. A byte-aligned call does not
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
