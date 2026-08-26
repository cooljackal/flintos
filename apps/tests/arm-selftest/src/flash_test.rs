// SPDX-License-Identifier: Apache-2.0

//! Destructive acceptance, limited to the board-reserved NVS partition.
//! GO=1 runs mutations; after a reset GO=2 verifies the persistent records.
//! The UART harness must detach SWD before releasing the delayed GO gate.

use api::task;
use hal::flash::NorFlash;
use hal::types::Priority;
use kernel::nvs::FlashStorage;
use kvstore::Store;
use portable_atomic::{AtomicU32, Ordering};
use rp2040_flash::Error;

#[no_mangle]
static FLASH_GO: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
static FLASH_STAGE: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
static FLASH_PEER_RUNS: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
static FLASH_PROGRAM_US: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
static FLASH_ERASE_US: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
static FLASH_TIMEOUT_US: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
static FLASH_WDT_BEFORE: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
static FLASH_WDT_AFTER: AtomicU32 = AtomicU32::new(0);
static PEER_COMMAND: AtomicU32 = AtomicU32::new(0);

fn output(bytes: &[u8]) {
    if let Some(console) = board::console() {
        console.write(bytes);
    }
}

fn check(ok: bool, code: u8) {
    if !ok {
        output(b"FLASH FAIL\r\n");
        super::fail(code);
    }
}

fn peer() {
    loop {
        FLASH_PEER_RUNS.fetch_add(1, Ordering::Relaxed);
        if PEER_COMMAND.load(Ordering::Acquire) == 1 {
            unsafe {
                core::arch::asm!("cpsid i", options(nostack));
            }
            PEER_COMMAND.store(2, Ordering::Release);
            let start = soc_rp2040::timer_us();
            while soc_rp2040::timer_us().wrapping_sub(start) < 80_000 {}
            unsafe {
                core::arch::asm!("cpsie i", options(nostack));
            }
            PEER_COMMAND.store(3, Ordering::Release);
        }
        if PEER_COMMAND.load(Ordering::Acquire) == 4 {
            let flash = unsafe { board::nvs_flash() }.expect("core-1 flash owner");
            check(
                unsafe { flash.write(flash.len() - 4, &[0x1711_beef]) }.is_ok(),
                64,
            );
            drop(flash);
            PEER_COMMAND.store(5, Ordering::Release);
        }
        task::yield_now();
    }
}

fn verify_keys(store: &Store<FlashStorage>) {
    let mut value = [0u8; 128];
    check(
        store.get(b"stable", &mut value) == Ok(11) && &value[..11] == b"pico-flash!",
        59,
    );
    check(
        store.get(b"counter", &mut value) == Ok(4) && value[..4] == 31u32.to_le_bytes(),
        59,
    );
    check(
        store.get(b"maximum-value", &mut value) == Ok(128) && value == [0x5a; 128],
        59,
    );
}

pub fn run() {
    FLASH_STAGE.store(1, Ordering::Release);
    output(b"FLASH READY\r\n");
    // Do not program while the flashing/debugging process is still detaching.
    while FLASH_GO.load(Ordering::Acquire) == 0 {
        task::sleep_ms(10);
    }
    task::sleep_ms(500);
    check(task::wait_until(|| kernel::smp::is_pinnable(1), 1_000), 50);
    task::spawn_on(1, "flash-peer", peer, Priority::Normal(1), 2048).expect("flash peer");
    check(
        task::wait_until(|| FLASH_PEER_RUNS.load(Ordering::Acquire) > 10, 1_000),
        50,
    );
    if matches!(FLASH_GO.load(Ordering::Acquire), 3 | 4) {
        FLASH_STAGE.store(8, Ordering::Release);
        if FLASH_GO.load(Ordering::Acquire) == 4 {
            unsafe {
                soc_rp2040::watchdog::arm(2_000, true);
            }
            // Enter with ~950 ms left after UART drain. The SRAM stall records
            // elapsed time in retained SRAM, independent of host latency.
            task::sleep_ms(1_000);
        }
        output(b"FLASH STALL\r\n");
        // Let UART drain before both cores stop. No flash cells are changed.
        task::sleep_ms(50);
        unsafe { rp2040_flash::inject_xip_stall() }
    }
    if FLASH_GO.load(Ordering::Acquire) == 2 {
        let store = Store::open(unsafe { FlashStorage::nvs() }).expect("persistent store");
        verify_keys(&store);
        FLASH_STAGE.store(7, Ordering::Release);
        output(b"FLASH PERSIST PASS\r\n");
        unsafe { soc_rp2040::test_status::pass_live() }
    }
    check(FLASH_GO.load(Ordering::Acquire) == 1, 50);
    FLASH_STAGE.store(2, Ordering::Release);
    let flash = unsafe { board::nvs_flash() }.expect("reserved flash");
    check(unsafe { board::nvs_flash() }.is_none(), 51);
    let mut word = [0];
    check(
        unsafe { flash.read(1, &mut word) } == Err(Error::Alignment),
        52,
    );
    check(
        unsafe { flash.write(flash.len(), &[0]) } == Err(Error::Range),
        52,
    );
    check(
        unsafe { flash.erase_sector(1) } == Err(Error::Alignment),
        52,
    );
    check(
        unsafe { flash.erase_sector(flash.len()) } == Err(Error::Range),
        52,
    );
    let masked = unsafe {
        core::arch::asm!("cpsid i", options(nostack));
        let result = soc_rp2040::xip::Guard::acquire().err();
        core::arch::asm!("cpsie i", options(nostack));
        result
    };
    check(masked == Some(soc_rp2040::xip::Error::InvalidContext), 53);
    PEER_COMMAND.store(1, Ordering::Release);
    check(
        task::wait_until(|| PEER_COMMAND.load(Ordering::Acquire) == 2, 1_000),
        53,
    );
    let start = soc_rp2040::timer_us();
    let blocked = unsafe { soc_rp2040::xip::Guard::acquire().err() };
    let elapsed = soc_rp2040::timer_us().wrapping_sub(start);
    FLASH_TIMEOUT_US.store(elapsed, Ordering::Release);
    check(
        blocked == Some(soc_rp2040::xip::Error::PeerTimeout) && (50_000..70_000).contains(&elapsed),
        53,
    );
    check(
        task::wait_until(|| PEER_COMMAND.load(Ordering::Acquire) == 3, 1_000),
        53,
    );
    // An unserviced UART RX DREQ leaves a real DMA channel busy. Flash must
    // refuse before XIP exit, and succeed after the channel is cancelled.
    let mut sink = [0u8; 4];
    let mut channel = soc_rp2040::dma::claim().expect("DMA exclusion fixture");
    channel
        .configure(soc_rp2040::dma::TransferConfig::peripheral_to_memory(
            soc_rp2040::UART1_BASE,
            sink.as_mut_ptr() as u32,
            4,
            soc_rp2040::dma::Dreq::UART1_RX,
        ))
        .expect("stalled RX channel");
    soc_rp2040::dma::start_mask(1 << channel.number()).expect("start stalled RX");
    check(
        unsafe { flash.erase_sector(0) }
            == Err(Error::Exclusion(soc_rp2040::xip::Error::DmaActive)),
        61,
    );
    channel.release().expect("release stalled DMA");
    let guard = unsafe { soc_rp2040::xip::Guard::acquire() }.expect("park/resume check");
    let stopped = FLASH_PEER_RUNS.load(Ordering::Acquire);
    let start = soc_rp2040::timer_us();
    while soc_rp2040::timer_us().wrapping_sub(start) < 2_000 {}
    let still_stopped = FLASH_PEER_RUNS.load(Ordering::Acquire) == stopped;
    drop(guard);
    check(still_stopped, 60);
    FLASH_STAGE.store(3, Ordering::Release);
    let start = soc_rp2040::timer_us();
    check(unsafe { flash.erase_all() }.is_ok(), 54);
    FLASH_ERASE_US.store(
        soc_rp2040::timer_us().wrapping_sub(start),
        Ordering::Release,
    );
    // A partial first page followed by a full page and partial last page.
    let mut pattern = [0u32; 100];
    for (i, word) in pattern.iter_mut().enumerate() {
        *word = 0x1710_0000 | i as u32;
    }
    let start = soc_rp2040::timer_us();
    check(unsafe { flash.write(252, &pattern) }.is_ok(), 55);
    FLASH_PROGRAM_US.store(
        soc_rp2040::timer_us().wrapping_sub(start),
        Ordering::Release,
    );
    let mut readback = [0u32; 100];
    check(
        unsafe { flash.read(252, &mut readback) }.is_ok() && readback == pattern,
        55,
    );
    check(
        unsafe { flash.read(248, &mut word) }.is_ok() && word == [u32::MAX],
        55,
    );
    check(
        unsafe { flash.read(652, &mut word) }.is_ok() && word == [u32::MAX],
        55,
    );
    check(
        unsafe { flash.write(252, &[0]) } == Err(Error::NotErased),
        55,
    );
    check(
        unsafe { flash.write(flash.len() - 4, &[0x1710_beef]) }.is_ok(),
        55,
    );
    check(unsafe { flash.erase_sector(0) }.is_ok(), 56);
    check(
        unsafe { flash.read(flash.len() - 4, &mut word) }.is_ok() && word == [0x1710_beef],
        56,
    );
    check(unsafe { flash.erase_all() }.is_ok(), 56);
    // Flash's temporary guard must preserve a caller's watchdog configuration
    // and consume, not replenish, its remaining deadline.
    unsafe {
        soc_rp2040::watchdog::arm(3_000, true);
    }
    let remaining_before = unsafe { (0x4005_8000 as *const u32).read_volatile() };
    FLASH_WDT_BEFORE.store(remaining_before, Ordering::Release);
    let result = unsafe { flash.erase_sector(0) };
    let remaining_after = unsafe { (0x4005_8000 as *const u32).read_volatile() };
    FLASH_WDT_AFTER.store(remaining_after, Ordering::Release);
    unsafe {
        soc_rp2040::watchdog::disarm();
        soc_rp2040::watchdog::clear_flint_watchdog_marker();
    }
    check(result.is_ok(), 63);
    // CTRL.TIME does not decrement on RP2040 (vendor issue #1492). Check policy
    // here; GO=4 separately proves the original running deadline was not fed.
    check(
        remaining_after & 0x4700_0000 == remaining_before & 0x4700_0000,
        62,
    );
    drop(flash);

    PEER_COMMAND.store(4, Ordering::Release);
    check(
        task::wait_until(|| PEER_COMMAND.load(Ordering::Acquire) == 5, 1_000),
        64,
    );
    let flash = unsafe { board::nvs_flash() }.expect("core-0 reclaimed flash");
    check(
        unsafe { flash.read(flash.len() - 4, &mut word) }.is_ok() && word == [0x1711_beef],
        64,
    );
    check(
        unsafe { flash.erase_sector(flash.len() - 4096) }.is_ok(),
        64,
    );
    drop(flash);

    FLASH_STAGE.store(4, Ordering::Release);
    let mut store = Store::open(unsafe { FlashStorage::nvs() }).expect("new store");
    check(store.set(b"stable", b"pico-flash!").is_ok(), 57);
    for count in 0u32..32 {
        check(store.set(b"counter", &count.to_le_bytes()).is_ok(), 57);
    }
    check(store.set(b"maximum-value", &[0x5a; 128]).is_ok(), 57);
    verify_keys(&store);
    let tail = store.used();
    drop(store);
    // Simulate a torn final header. This is not a physical power-cut test.
    let flash = unsafe { board::nvs_flash() }.expect("raw torn-tail writer");
    check(unsafe { flash.write(tail, &[0x0403_a5c3]) }.is_ok(), 58);
    drop(flash);
    let mut store = Store::open(unsafe { FlashStorage::nvs() }).expect("torn-tail reopen");
    verify_keys(&store);
    check(store.set(b"new", b"value") == Err(kvstore::Error::Io), 58);
    // Existing compaction is an explicit erase/rewrite, NOT power-loss atomic.
    check(store.compact(&mut [0u8; 512]).is_ok(), 58);
    verify_keys(&store);
    drop(store);
    FLASH_STAGE.store(5, Ordering::Release);
    let before = FLASH_PEER_RUNS.load(Ordering::Acquire);
    check(
        task::wait_until(
            || FLASH_PEER_RUNS.load(Ordering::Acquire) > before + 10,
            1_000,
        ),
        60,
    );
    FLASH_STAGE.store(6, Ordering::Release);
    output(b"FLASH WRITE PASS\r\n");
    unsafe { soc_rp2040::test_status::pass_live() }
}
