// SPDX-License-Identifier: Apache-2.0

use super::{Error, PAGE_SIZE, SECTOR_SIZE};

static mut CLAIMED: bool = false;
const CLAIM_LOCK: *mut u32 = 0xd000_017c as *mut u32;

unsafe fn ownership(value: bool) -> bool {
    let mask: u32;
    core::arch::asm!("mrs {}, PRIMASK", "cpsid i", out(reg) mask, options(nostack));
    while CLAIM_LOCK.read_volatile() == 0 {
        core::hint::spin_loop();
    }
    core::arch::asm!("dmb", options(nostack));
    let slot = core::ptr::addr_of_mut!(CLAIMED);
    let previous = slot.read();
    slot.write(value);
    core::arch::asm!("dmb", options(nostack));
    CLAIM_LOCK.write_volatile(1);
    core::arch::asm!("msr PRIMASK, {}", in(reg) mask, options(nostack));
    previous
}

pub unsafe fn claim() -> bool {
    !ownership(true)
}
pub unsafe fn release() {
    ownership(false);
}

type RomFn = unsafe extern "C" fn();
type EraseFn = unsafe extern "C" fn(u32, usize, u32, u8);
type ProgramFn = unsafe extern "C" fn(u32, *const u8, usize);

// Early watchdog recovery enters ROM before the application's BSS clear.
// Scratch[0..3] were zero after recovery on this fixture; retain timing in SRAM.
#[cfg(feature = "flash-fault-injection")]
#[no_mangle]
pub static mut FLASH_STALL_MAGIC: u32 = 0;
#[cfg(feature = "flash-fault-injection")]
#[no_mangle]
pub static mut FLASH_STALL_US: u32 = 0;

struct Rom {
    connect: RomFn,
    exit: RomFn,
    erase: EraseFn,
    program: ProgramFn,
    flush: RomFn,
}

unsafe fn lookup(code: [u8; 2]) -> Result<usize, Error> {
    type Lookup = unsafe extern "C" fn(*const u16, u32) -> usize;
    let table = (0x14 as *const u16).read_volatile() as *const u16;
    let lookup: Lookup = core::mem::transmute((0x18 as *const u16).read_volatile() as usize);
    let address = lookup(table, u16::from_le_bytes(code) as u32);
    if address & 1 == 0 || address >= 0x4000 {
        return Err(Error::RomUnavailable);
    }
    Ok(address)
}

pub unsafe fn operate(offset: u32, page: Option<&[u32; PAGE_SIZE / 4]>) -> Result<(), Error> {
    let _exclusion = soc_rp2040::xip::Guard::acquire().map_err(Error::Exclusion)?;
    // Resolve all ROM addresses and copy boot2 while XIP is still available.
    let rom = Rom {
        connect: core::mem::transmute::<usize, RomFn>(lookup(*b"IF")?),
        exit: core::mem::transmute::<usize, RomFn>(lookup(*b"EX")?),
        erase: core::mem::transmute::<usize, EraseFn>(lookup(*b"RE")?),
        program: core::mem::transmute::<usize, ProgramFn>(lookup(*b"RP")?),
        flush: core::mem::transmute::<usize, RomFn>(lookup(*b"FC")?),
    };
    let mut boot2 = [0u32; 64];
    for (i, word) in boot2.iter_mut().enumerate() {
        *word = (soc_rp2040::XIP_BASE as *const u32).add(i).read_volatile();
    }
    let boot: RomFn = core::mem::transmute(boot2.as_ptr() as usize | 1);
    let _deadline = soc_rp2040::watchdog::FlashDeadline::begin().ok_or(Error::WatchdogTimebase)?;
    execute_rom(
        &rom,
        boot,
        offset,
        page.map_or(core::ptr::null(), |p| p.as_ptr().cast()),
    );
    // Drop order restores the watchdog before releasing the other core.
    Ok(())
}

/// Every instruction, literal and call target between exit and boot is SRAM
/// or ROM. Keep out-of-line: inlining into a flash caller silently breaks this.
/// Pico SDK hardware_flash/flash.c supplies the save/restore sequence.
#[inline(never)]
#[link_section = ".ram_func.flash_rom"]
unsafe fn execute_rom(rom: &Rom, boot: RomFn, offset: u32, page: *const u8) {
    // Initializing this array to zero makes size-optimized LLVM emit a call
    // to flash-resident __aeabi_memclr4. Every slot is filled before reading.
    let mut pad_storage = core::mem::MaybeUninit::<[u32; 6]>::uninit();
    let pads = pad_storage.as_mut_ptr().cast::<u32>();
    let mut i = 0usize;
    while i < 6 {
        pads.add(i)
            .write((0x4002_0004 as *const u32).add(i).read_volatile());
        i = i.wrapping_add(1);
    }
    let xip_ctrl = (0x1400_0000 as *const u32).read_volatile();
    core::arch::asm!("dsb", "isb", options(nostack));
    (rom.connect)();
    (rom.exit)();
    if page.is_null() {
        (rom.erase)(offset, SECTOR_SIZE as usize, SECTOR_SIZE, 0x20);
    } else {
        (rom.program)(offset, page, PAGE_SIZE);
    }
    (rom.flush)();
    boot();
    i = 0;
    while i < 6 {
        (0x4002_0004 as *mut u32)
            .add(i)
            .write_volatile(pads.add(i).read());
        i = i.wrapping_add(1);
    }
    (0x1400_0000 as *mut u32).write_volatile(xip_ctrl);
    core::arch::asm!("dsb", "isb", options(nostack));
}

/// Deliberately stop after removing XIP, without changing any flash cells.
///
/// # Safety
/// Destructive test only: commits to a watchdog reset. Both cores must have
/// the cooperating IRQ, and no debugger/NMI may access XIP until the reset.
#[cfg(feature = "flash-fault-injection")]
pub unsafe fn inject_xip_stall() -> ! {
    let _exclusion = soc_rp2040::xip::Guard::acquire().expect("fault fixture exclusion");
    let connect = core::mem::transmute::<usize, RomFn>(lookup(*b"IF").expect("connect ROM"));
    let exit = core::mem::transmute::<usize, RomFn>(lookup(*b"EX").expect("exit ROM"));
    let _deadline = soc_rp2040::watchdog::FlashDeadline::begin().expect("fault fixture deadline");
    stall_in_ram(connect, exit)
}

#[cfg(feature = "flash-fault-injection")]
#[inline(never)]
#[link_section = ".ram_func.flash_stall"]
unsafe fn stall_in_ram(connect: RomFn, exit: RomFn) -> ! {
    connect();
    exit();
    let timer = 0x4005_4028 as *const u32;
    let started = timer.read_volatile();
    core::ptr::addr_of_mut!(FLASH_STALL_MAGIC).write_volatile(0x171f_dead);
    loop {
        let elapsed = timer.read_volatile().wrapping_sub(started);
        // PSM may reset TIMER just before the processors. Do not overwrite
        // the last valid elapsed sample with that reset-induced wrap.
        if elapsed < 10_000_000 {
            core::ptr::addr_of_mut!(FLASH_STALL_US).write_volatile(elapsed);
        }
    }
}
