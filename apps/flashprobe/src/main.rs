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

fn main() {
    task::spawn("probe", run, Priority::Normal(2), 4096);
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

    // The struct is fine, so the ROM is not the problem. Try the real thing
    // from here, where the scheduler is running and a task has a stack.
    match round_trip() {
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

    {
        use core::sync::atomic::Ordering;
        for i in 0..4 {
            let v = esp32_flash::STATUS_TRACE[i].load(Ordering::Relaxed);
            api::log_info!("[probe] status[{}] = {:#05x}", i, v);
        }
    }

    // 5: read back.
    let mut out = [0u8; 32];
    let n = store.get(b"probe.a", &mut out).map_err(|_| "read back")?;
    if &out[..n] != b"first" {
        return Err("value came back wrong");
    }
    api::log_info!("[probe] read back {} bytes", n);
    Ok(())
}
