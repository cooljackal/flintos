// SPDX-License-Identifier: Apache-2.0

//! Prints what the ROM believes about the flash, and nothing else.
//!
//! The self-test harness runs in boot context before the scheduler, and three
//! sessions of flash debugging there have produced one confusing result after
//! another — most recently a board that died partway through printing a string
//! literal, before reading anything at all.
//!
//! So this is the same read from an ordinary task in an ordinary app. If it
//! works here, the harness was the problem and the flash driver may be fine.
//! If it dies here too, the fault is in the read itself and everything built
//! on top of it has been chasing a symptom.
//!
//! Deliberately does not touch flash. No cache changes, no ROM calls — just
//! six words of DRAM that the bootloader is supposed to have filled in.

#![no_std]
#![no_main]

use api::task;
use hal::types::Priority;

kernel::flint_app!(main, abi = 1);

/// Bumped by a task on the *second* core, so a flash operation can be checked
/// against a core that is genuinely running rather than one parked at boot.
static CORE1_TICKS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

fn main() {
    // Start the APP CPU and bring it into the scheduler. Without this the
    // second core is stalled from reset, `with_cache_off` sees nothing to
    // stall, and the cross-core path -- the whole point of #69's second half
    // -- is never exercised.
    unsafe {
        arch_xtensa::appcpu::prepare(second_core);
        soc_esp32::appcpu::start(arch_xtensa::appcpu::_flint_appcpu_entry);
    }
    task::spawn_on(1, "core1", core1_counter, Priority::Normal(2), 4096);
    task::spawn("probe", run, Priority::Normal(2), 4096);
}

/// The second core's entry. In IRAM: it runs before reaching anything in
/// flash, and its cache is enabled from the other core.
#[link_section = ".iram1.second_core"]
extern "C" fn second_core() -> ! {
    unsafe { kernel::boot::join_scheduler() }
}

/// Something for core 1 to be doing while core 0 erases and writes flash.
fn core1_counter() {
    loop {
        CORE1_TICKS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        task::sleep_ms(1);
    }
}

fn run() {
    task::sleep_ms(200);
    api::log_info!("[probe] reading the ROM's flash chip struct at 0x3FFAE270");

    let c = unsafe { esp32_flash::ChipInfo::read() };
    api::log_info!("[probe] device_id   = {:#010x}", c.device_id);
    api::log_info!("[probe] chip_size   = {:#010x}", c.chip_size);
    api::log_info!("[probe] block_size  = {:#010x}", c.block_size);
    api::log_info!("[probe] sector_size = {:#010x}", c.sector_size);
    api::log_info!("[probe] page_size   = {:#010x}", c.page_size);
    api::log_info!("[probe] status_mask = {:#010x}", c.status_mask);

    if c.looks_sane() {
        api::log_info!("[probe] the ROM knows its chip — 256/4096 geometry is right");
    } else {
        api::log_error!("[probe] not populated: the ROM would build commands from this");
    }

    // What the bootloader left SPI1 configured as. Every theory about where an
    // extra clock comes from has been an inference from behaviour; this is the
    // configuration itself.
    for (name, off) in [
        ("CTRL ", 0x08u32), ("CTRL1", 0x0c), ("CTRL2", 0x14), ("CLOCK", 0x18),
        ("USER ", 0x1c), ("USER1", 0x20), ("USER2", 0x24), ("PIN  ", 0x34),
    ] {
        let v1 = unsafe { ((0x3FF4_2000u32 + off) as *const u32).read_volatile() };
        // SPI0 is the cache's controller and demonstrably reads this chip
        // correctly, so wherever the two differ is a candidate.
        let v0 = unsafe { ((0x3FF4_3000u32 + off) as *const u32).read_volatile() };
        api::log_info!("[probe] {} SPI1={:#010x} SPI0={:#010x}", name, v1, v0);
    }

    // The struct is fine, so the ROM is not the problem. Try the real thing
    // from here, where the scheduler is running and a task has a stack.
    let outcome = round_trip();
    {
        use core::sync::atomic::Ordering;
        for i in 0..4 {
            let v = esp32_flash::STATUS_TRACE[i].load(Ordering::Relaxed);
            api::log_info!("[probe] status[{}] = {:#05x}", i, v);
        }
    }
    match outcome {
        Ok(()) => api::log_info!("[probe] PASS"),
        Err(e) => api::log_error!("[probe] FAIL: {}", e),
    }

    loop {
        task::sleep_ms(1000);
    }
}

/// How far to go. Bisecting: each step adds one thing to the previous.
///
/// 0 does nothing at all. If that dies, the fault is not in this function's
/// body — it is in the call, or in something linking the flash driver does.
const STEP: u8 = 5;

fn round_trip() -> Result<(), &'static str> {
    use kernel::nvs::FlashStorage;
    use kvstore::Store;

    api::log_info!("[probe] step {} begin", STEP);
    if STEP == 0 {
        return Ok(());
    }

    // 1: construct the storage handle. Touches no flash.
    let storage = unsafe { FlashStorage::nvs() };
    api::log_info!("[probe] storage constructed");
    if STEP == 1 {
        return Ok(());
    }

    // 2: open, which scans — the first flash read.
    let mut store = Store::open(storage).map_err(|_| "open")?;
    api::log_info!("[probe] opened, {} bytes used", store.used());
    if STEP == 2 {
        return Ok(());
    }

    // The cross-core check. Core 1 is running kernel tasks on the other side
    // of a hardware stall: if `with_cache_off` fails to stall it, it keeps
    // fetching through a cache that is about to go away and stops dead; if it
    // fails to *release* it, it never runs again. Either way this counter
    // stops moving, and either way the board otherwise looks fine.
    let before_core1 = CORE1_TICKS.load(core::sync::atomic::Ordering::Relaxed);
    if before_core1 == 0 {
        api::log_error!("[probe] core 1 never started -- the cross-core path is untested");
    }

    // 3: erase.
    store.erase_all().map_err(|_| "erase")?;
    api::log_info!("[probe] erased");
    if STEP == 3 {
        return Ok(());
    }

    // 4: write.
    store.set(b"probe.a", b"first").map_err(|_| "write")?;
    api::log_info!("[probe] wrote");
    if STEP == 4 {
        return Ok(());
    }

    // Straight at the driver, away from kvstore: a known pattern into an
    // erased part of the same sector, then back. kvstore's header is the only
    // thing that has ever looked wrong, and this says whether the driver or
    // the format is responsible.
    {
        use kvstore::Storage;
        let mut st = unsafe { FlashStorage::nvs() };
        let pattern: [u8; 16] = [
            0xc3, 0xa5, 0x07, 0x05, 0x11, 0x22, 0x33, 0x44,
            0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc,
        ];
        match st.write(0x100, &pattern) {
            Ok(()) => {
                let mut back = [0u8; 16];
                match st.read(0x100, &mut back) {
                    Ok(()) => {
                        api::log_info!("[probe] direct wrote {:02x?}", pattern);
                        api::log_info!("[probe] direct read  {:02x?}", back);
                    }
                    Err(_) => api::log_error!("[probe] direct read failed"),
                }
            }
            Err(_) => api::log_error!("[probe] direct write failed"),
        }
    }

    // Dump what actually landed, before asking kvstore to interpret it.
    {
        use kvstore::Storage;
        let st = unsafe { FlashStorage::nvs() };
        let mut raw = [0u8; 24];
        match st.read(0, &mut raw) {
            Ok(()) => api::log_info!("[probe] raw@0 {:02x?}", raw),
            Err(_) => api::log_error!("[probe] raw read failed"),
        }
    }

    {
        use core::sync::atomic::Ordering;
        let names = ["CMD ", "ADDR", "CTRL", "USER", "USR1", "MIDL", "MODL", "RDST"];
        for i in 0..8 {
            let a = esp32_flash::REG_SNAPSHOT[i].load(Ordering::Relaxed);
            let z = esp32_flash::REG_SNAPSHOT[8 + i].load(Ordering::Relaxed);
            if a != z {
                api::log_info!("[probe] DIFF {} first={:#010x} later={:#010x}", names[i], a, z);
            }
        }
    }

    // 5: read back.
    let mut out = [0u8; 32];
    let n = store.get(b"probe.a", &mut out).map_err(|_| "read back")?;
    if &out[..n] != b"first" {
        return Err("value came back wrong");
    }
    api::log_info!("[probe] read back {} bytes", n);

    // Give core 1 a few of its milliseconds to prove it survived.
    task::sleep_ms(20);
    let after_core1 = CORE1_TICKS.load(core::sync::atomic::Ordering::Relaxed);
    if after_core1 > before_core1 {
        api::log_info!(
            "[probe] core 1 ran across the flash writes: {} -> {}",
            before_core1,
            after_core1
        );
    } else {
        api::log_error!(
            "[probe] core 1 STOPPED at {} -- stall or release is wrong",
            after_core1
        );
        return Err("core 1 did not survive the flash operation");
    }
    Ok(())
}
