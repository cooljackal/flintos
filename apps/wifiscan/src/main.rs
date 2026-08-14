// SPDX-License-Identifier: Apache-2.0

//! Scans for access points and prints them. Step 5.2 of `doc/plan-radio.md`.
//!
//! The first FlintOS binary in which the radio *does* something rather than
//! merely coming up. [`radioprobe`] proved the PHY registers and
//! `esp_wifi_init_internal` returns `ESP_OK`; this proves the driver actually
//! runs — its task is scheduled, its interrupts arrive, its timers fire, and
//! it hands back data that came off the air.
//!
//! # What a scan exercises that init does not
//!
//! Init allocates and creates objects. A scan *runs* the driver: `wifiT` has
//! to be scheduled by FlintOS's scheduler at the priority the adapter gave it,
//! the MAC interrupt has to reach the handler `_set_isr` installed, the
//! software timers behind `_timer_arm` have to fire on time, and
//! `_event_post` has to deliver the completion. Every one of those is an OSI
//! entry that init only ever touched, never depended on.
//!
//! # Running it
//!
//! ```text
//! make flash APP=wifiscan BOARD=board-esp32-devkitc EXTRA_FEATURES=blobs
//! ```
//!
//! It needs no configuration and no credentials: an all-channel active scan
//! reports whatever is in the room. Expect it to take about a second and a
//! half — thirteen channels at the driver's default dwell — and to find fewer
//! networks than a phone does, because a phone scans repeatedly and merges.
//!
//! [`radioprobe`]: ../radioprobe/index.html

#![no_std]
#![no_main]

use api::task;
use hal::types::Priority;

kernel::flint_app!(main, abi = 1);

fn main() {
    // As `radioprobe`: the blob's frame depth is not knowable from here, and
    // this application has one task to spend the pool on.
    task::spawn("wifiscan", run, Priority::Normal(2), 16384);
}

/// The build with no archives to call. See `radioprobe` for why this is a
/// `cfg`-ed body rather than a `compile_error!`.
#[cfg(not(feature = "blobs"))]
fn run() {
    loop {
        api::log_warn!("[wifi] built without the blobs; there is nothing to scan with");
        api::log_warn!("[wifi]   make blobs");
        api::log_warn!("[wifi]   make flash APP=wifiscan BOARD=... EXTRA_FEATURES=blobs");
        task::sleep_ms(5000);
    }
}

/// How many results to copy out of the driver at once.
///
/// A busy office sees more than this. The count found is printed alongside the
/// count shown, so a truncated list says so rather than looking complete —
/// which matters because `scan::ap_records` frees the driver's list either
/// way, so the overflow is genuinely gone rather than waiting.
#[cfg(feature = "blobs")]
const MAX_RESULTS: usize = 24;

/// What the event handler saw, for the main task to read.
///
/// An atomic because the two run on different tasks: the handler on
/// `radio-event`, this on the application's. See
/// `radio_esp32::events::set_handler`.
#[cfg(feature = "blobs")]
static EVENTS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Set when `WIFI_EVENT_SCAN_DONE` arrives.
#[cfg(feature = "blobs")]
static SCAN_DONE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The driver's event callback.
///
/// Runs on `radio-event`, on its own stack, with nothing of the driver's held
/// — so it may call back into `esp_wifi_*`, and does. That is the whole reason
/// the queue exists, and it is where Zephyr reads its scan results too.
#[cfg(feature = "blobs")]
fn on_event(base: *const core::ffi::c_char, id: i32, _data: *mut core::ffi::c_void, len: usize) {
    use core::sync::atomic::Ordering;
    EVENTS.fetch_add(1, Ordering::Relaxed);

    // Compared by pointer, not by string: `esp_event_base_t` is an identity,
    // and esp-idf's own handlers compare it the same way.
    let known = core::ptr::eq(base, radio_esp32::wifi::WIFI_EVENT.0);
    let name = match id {
        radio_esp32::wifi::event::WIFI_READY => "wifi-ready",
        radio_esp32::wifi::event::SCAN_DONE => "scan-done",
        radio_esp32::wifi::event::STA_START => "sta-start",
        radio_esp32::wifi::event::STA_STOP => "sta-stop",
        radio_esp32::wifi::event::STA_CONNECTED => "sta-connected",
        radio_esp32::wifi::event::STA_DISCONNECTED => "sta-disconnected",
        _ => "?",
    };
    api::log_info!(
        "[wifi] event {} id={} ({}) {} bytes",
        if known { "WIFI_EVENT" } else { "other" },
        id,
        name,
        len
    );

    if known && id == radio_esp32::wifi::event::SCAN_DONE {
        SCAN_DONE.store(true, Ordering::SeqCst);
        report_results();
    }
}

/// Read the scan out and print it. **Called from the event handler**, which is
/// what Zephyr does and what the driver expects: the results belong to
/// whoever handled `SCAN_DONE`, and reading them from another task after a
/// blocking scan is a pattern neither reference uses.
#[cfg(feature = "blobs")]
fn report_results() {
    use radio_esp32::scan;

    let found = match unsafe { scan::ap_count() } {
        Ok(n) => n,
        Err(e) => {
            api::log_error!("[wifi] ap_count failed: {:#x}", e);
            return;
        }
    };

    // Zeroed rather than `MaybeUninit`: 80 bytes times 24 is under 2 KiB
    // against the event task's 8 KiB, and the driver writes every field it
    // reports. The cost is one memset per scan against a class of bug that
    // only appears when it returns fewer records than it said it would.
    let mut records = [scan::ApRecord {
        bssid: [0; 6],
        ssid: [0; 33],
        primary: 0,
        second: 0,
        rssi: 0,
        authmode: 0,
        pairwise_cipher: 0,
        group_cipher: 0,
        ant: 0,
        flags: 0,
        country: scan::Country { cc: [0; 3], schan: 0, nchan: 0, max_tx_power: 0, policy: 0 },
    }; MAX_RESULTS];

    let shown = match unsafe { scan::ap_records(&mut records) } {
        Ok(n) => n as usize,
        Err(e) => {
            api::log_error!("[wifi] ap_records failed: {:#x}", e);
            return;
        }
    };

    if found as usize > shown {
        api::log_warn!("[wifi] {} networks, showing {} (raise MAX_RESULTS)", found, shown);
    } else {
        let mut fired = 0;
        radio_esp32::interrupts::for_each_route(|r| {
            fired += radio_esp32::interrupts::fires(r.num as usize);
        });
        api::log_info!("[wifi] {} networks, {} radio interrupts", found, fired);
        // Short, deliberately: a long line here killed the event task once.
        api::log_info!(
            "[wifi] phy {} on, {} off",
            radio_esp32::adapter::PHY_ENABLES.load(core::sync::atomic::Ordering::Relaxed),
            radio_esp32::adapter::PHY_DISABLES.load(core::sync::atomic::Ordering::Relaxed)
        );
        osi_calls("scan");
        alloc_report("scan");
    }

    for r in &records[..shown] {
        let b = r.bssid;
        // Printed as a byte count when the SSID is not UTF-8, which is legal
        // 802.11 and does happen.
        match r.ssid_str() {
            Some("") => api::log_info!(
                "[wifi]   {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  ch{:<3} {:>4} dBm  {:<9} <hidden>",
                b[0], b[1], b[2], b[3], b[4], b[5],
                r.primary, r.rssi, scan::auth_name(r.authmode)
            ),
            Some(name) => api::log_info!(
                "[wifi]   {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  ch{:<3} {:>4} dBm  {:<9} {}",
                b[0], b[1], b[2], b[3], b[4], b[5],
                r.primary, r.rssi, scan::auth_name(r.authmode), name
            ),
            None => api::log_info!(
                "[wifi]   {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  ch{:<3} {:>4} dBm  {:<9} <{} non-utf8 bytes>",
                b[0], b[1], b[2], b[3], b[4], b[5],
                r.primary, r.rssi, scan::auth_name(r.authmode), r.ssid_bytes().len()
            ),
        }
    }
}

/// How far `run` has got. Read by [`watchdog`] when `init` does not return.
#[cfg(feature = "blobs")]
static STAGE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Print the OSI call counts, two entries to a line.
///
/// Two to a line because a long one killed the event task once, and this is
/// called from the event handler as well as the application task.
#[cfg(feature = "blobs")]
fn osi_calls(stage: &str) {
    use radio_esp32::adapter::calls;
    let mut i = 0;
    while i < calls::N {
        let b = i + 1;
        if b < calls::N {
            api::log_info!(
                "[wifi] osi/{} {}={} {}={}",
                stage,
                calls::NAMES[i],
                calls::get(i),
                calls::NAMES[b],
                calls::get(b)
            );
        } else {
            api::log_info!("[wifi] osi/{} {}={}", stage, calls::NAMES[i], calls::get(i));
        }
        i += 2;
    }
}

/// Fault injection: hold the driver's own threads at the door this long
/// before they enter its code. **Zero for a normal build.**
///
/// The start-up hang is roughly one boot in three, which is a poor rate to
/// study by rebooting. If it is an ordering race around the first handshake,
/// moving when the worker starts should move the failure rate; a sharp
/// threshold would point at a timeout instead, and no change would weaken
/// this particular theory. Several values, several boots each — one boot can
/// expose a failure but says nothing about timing.
#[cfg(feature = "blobs")]
const WORKER_START_DELAY_MS: u32 = 0;

/// The control: the same wait, somewhere that is not the worker's ordering.
///
/// Without it, a change in failure rate could be the delay disturbing timing
/// generally rather than the worker's position in particular. Set this to the
/// same value as the injection on a control run, and the injection to zero.
#[cfg(feature = "blobs")]
const CONTROL_DELAY_MS: u32 = 0;

/// Print what the driver has allocated and still holds.
///
/// Counts alone hide the live set, so this reports each outstanding block with
/// the entry that asked for it, its size and its address, plus totals and the
/// two ways the trace can under-report: records lost once the table filled,
/// and frees that matched nothing recorded.
#[cfg(feature = "blobs")]
fn alloc_report(stage: &str) {
    use radio_esp32::adapter;
    let (seen, live, bytes, lost, stray) = adapter::alloc_totals();
    api::log_info!(
        "[wifi] mem/{} {} seen, {} live, {} bytes",
        stage, seen, live, bytes
    );
    if lost > 0 || stray > 0 {
        api::log_warn!("[wifi] mem/{} {} lost, {} stray frees", stage, lost, stray);
    }
    let mut shown = 0;
    adapter::for_each_live_alloc(|a| {
        // Bounded: the console is the bottleneck, and the large blocks are
        // what the receive path would need.
        if shown < 20 && a.size >= 64 {
            api::log_info!(
                "[wifi] mem/{} p{} {} {} at {:#x}",
                stage,
                a.phase,
                adapter::ALLOC_NAMES[a.kind as usize],
                a.size,
                a.ptr
            );
            shown += 1;
        }
    });
}

/// Print every task the driver asked for, and whether it ever ran.
///
/// The question: does the driver ever ask for the second core? That core is
/// not started here, and a task pinned to it would be created, reported as a
/// success, and then never run — which would explain both the hang during
/// start-up and the receiver never being switched on. Entirely unconfirmed
/// until one of these lines shows a core of 1.
#[cfg(feature = "blobs")]
fn task_report(stage: &str) {
    radio_esp32::tasks::for_each_create(|c| {
        let (entered, exited) = radio_esp32::tasks::slot_counts(c.slot as usize);
        api::log_info!(
            "[wifi] task/{} slot {} core {} -> {:?} prio {} stack {}",
            stage,
            c.slot,
            c.core_id,
            c.pinned_to,
            c.prio,
            c.stack
        );
        api::log_info!(
            "[wifi] task/{} slot {} spawned {} ran {} returned {}",
            stage,
            c.slot,
            c.spawned,
            entered,
            exited
        );
    });
}

/// Which task is running `run`, so [`watchdog`] can ask what it is blocked on.
#[cfg(feature = "blobs")]
static RUN_TASK: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(u32::MAX);

/// Report the stage every second, from a task that `init` cannot block.
///
/// The stage alone said "blocked, not dead". What it could not say is *on
/// what*: every blocking primitive the blob gets — queue send and receive,
/// semaphore take, mutex lock — funnels through `kernel::queue::block_*`, and
/// that now records the object's address, which `radio_esp32::adapter::describe`
/// turns back into "the fourth semaphore the driver created".
#[cfg(feature = "blobs")]
fn watchdog() {
    for i in 1..=8 {
        task::sleep_ms(1000);
        let stage = STAGE.load(core::sync::atomic::Ordering::SeqCst);
        if stage >= 2 {
            return;
        }
        let who = RUN_TASK.load(core::sync::atomic::Ordering::SeqCst);
        match kernel::queue::waiting_on(who) {
            Some(w) => {
                let what = radio_esp32::adapter::describe(w.addr);
                api::log_info!(
                    "[wifi] watch {}: stage {}, blocked on {} {} at {:#x}, {} since tick {}",
                    i,
                    stage,
                    what.map(|d| d.kind).unwrap_or("unknown object"),
                    what.map(|d| d.nth).unwrap_or(0),
                    w.addr,
                    if w.send { "send" } else { "recv" },
                    w.since
                );
                if w.timeout_ms == u32::MAX {
                    api::log_info!("[wifi] watch {}:   timeout is FOREVER", i);
                }
            }
            None => api::log_info!(
                "[wifi] watch {}: stage {}, not blocked in a queue primitive",
                i, stage
            ),
        }
        // Every other task too, not just this one. The driver's worker does
        // enter its entry point on a hung boot -- measured -- so what it is
        // itself waiting for is the question, and it is the same record.
        for id in 0..kernel::MAX_TASKS as u32 {
            if id == who {
                continue;
            }
            if let Some(w) = kernel::queue::waiting_on(id) {
                let what = radio_esp32::adapter::describe(w.addr);
                api::log_info!(
                    "[wifi] watch {}: task {} on {} {} {}",
                    i,
                    id,
                    what.map(|d| d.kind).unwrap_or("unknown"),
                    what.map(|d| d.nth).unwrap_or(0),
                    if w.send { "send" } else { "recv" }
                );
            }
        }
        // The counts matter most on the boot that hangs, and that boot never
        // reaches the dump after `driver up`.
        if i == 1 {
            osi_calls("hang");
            task_report("hang");
        }
    }
}

/// Fill the `nvs` partition through the driver's own C entry points until it
/// has to compact, then report what the compaction path did.
///
/// This is a diagnostic, and it writes junk into the partition that only
/// `make erase` clears. It stays off unless something is being measured.
#[cfg(feature = "blobs")]
const NVS_FILL_PROBE: bool = false;

/// Stop filling once the log reaches this many bytes. Zero fills it right up.
///
/// 10000 is just above where five boots out of five hung; filling to it on the
/// way past means the hang reproduces on the boot that creates the condition,
/// instead of needing a second one.
#[cfg(feature = "blobs")]
const FILL_TARGET: u32 = 10_000;

/// Drive `nvs_set_blob` past the end of the log.
///
/// It goes through the C shim rather than `with_store`, because the thing in
/// question is the *wiring* -- `nvs_set_blob` -> `set_tagged` -> `put`'s
/// `Full` arm -> `compact` -> retry -- and calling `Store::set` directly would
/// skip every step of it. One key is written over and over, so the live set
/// stays at a single entry and a compaction that runs must reclaim nearly the
/// whole partition.
#[cfg(feature = "blobs")]
fn fill_probe() {
    use core::ffi::{c_char, c_void};

    extern "C" {
        fn nvs_open(name: *const c_char, mode: u32, out: *mut c_void) -> i32;
        fn nvs_set_blob(handle: u32, key: *const c_char, value: *const c_void, len: usize) -> i32;
    }

    let mut handle: u32 = 0;
    let rc = unsafe {
        nvs_open(
            b"probe\0".as_ptr() as *const c_char,
            1,
            (&mut handle) as *mut u32 as *mut c_void,
        )
    };
    if rc != 0 {
        api::log_error!("[wifi] probe: nvs_open rc={:#x}", rc);
        return;
    }

    let payload = [0xa5u8; 100];
    let mut writes = 0u32;
    let mut last = 0i32;
    // Enough to fill a 24 KiB log twice over, so a second compaction would
    // show up as well as the first. It stops early at [`FILL_TARGET`] when
    // that is set, which is how the init hang is reproduced on one boot
    // rather than two.
    for _ in 0..400 {
        if FILL_TARGET > 0 && radio_esp32::nvs::with_store(|s| s.used(), 0) >= FILL_TARGET {
            break;
        }
        last = unsafe {
            nvs_set_blob(
                handle,
                b"fill\0".as_ptr() as *const c_char,
                payload.as_ptr() as *const c_void,
                payload.len(),
            )
        };
        if last != 0 {
            break;
        }
        writes += 1;
    }

    let used = radio_esp32::nvs::with_store(|s| s.used(), 0);
    let p = radio_esp32::nvs::probe();
    api::log_info!(
        "[wifi] probe: {} writes, last rc={:#x}, used {}",
        writes, last, used
    );
    // Two lines: the console truncates a long one, and a counter that is cut
    // off is the same as a counter that was never printed.
    api::log_info!(
        "[wifi] probe: set_full={} no_heap={} compact_err={}",
        p.set_full, p.no_heap, p.compact_err
    );
    api::log_info!(
        "[wifi] probe: compacted={} reclaimed={} retry_ok={} retry_err={}",
        p.compacted, p.reclaimed, p.retry_ok, p.retry_err
    );
}

#[cfg(feature = "blobs")]
fn run() {
    use radio_esp32::wifi;

    task::sleep_ms(200);
    api::log_info!("[wifi] starting");

    // The same bring-up `radioprobe` does, in the same order and for the same
    // reasons: NVS so a calibration can persist, then the heap, because the
    // driver allocates everything out of it through the OSI table.
    if !unsafe { radio_esp32::nvs::init() } {
        api::log_warn!("[wifi] nvs unavailable; every boot will calibrate in full");
    }
    let heap = unsafe { kernel::heap::init_from_map() };
    api::log_info!("[wifi] heap: {} bytes", heap);

    // How full the append-only log is, and what one read of it costs. The
    // init hang tracks this number; see doc/plan-radio.md.
    let (used, free) = radio_esp32::nvs::with_store(|s| (s.used(), s.free()), (0, 0));
    let t = kernel::clock::now_us();
    let probe = radio_esp32::nvs::with_store(
        |s| { let mut b = [0u8; 128]; s.get(b"rfcal.v", &mut b).map(|n| n as i32).unwrap_or(-1) },
        -2,
    );
    api::log_info!(
        "[wifi] nvs: {} used, {} free, one get = {} us (rc {})",
        used, free, kernel::clock::now_us() - t, probe
    );
    if NVS_FILL_PROBE {
        fill_probe();
    }
    // Before the driver reads anything out of it. The log accumulates a
    // superseded calibration every boot, and what a read costs is what the
    // log is long. See `radio_esp32::nvs::compact_if_grown`.
    if radio_esp32::nvs::compact_if_grown() {
        let (used, free) = radio_esp32::nvs::with_store(|s| (s.used(), s.free()), (0, 0));
        api::log_info!("[wifi] nvs compacted at boot: {} used, {} free", used, free);
    }
    // Every one of those reads ran with the cache off and the second core out
    // of the way. It is *asked* to park first; a fallback hardware stall is
    // what neither esp-idf nor NuttX does, and a stalled core can be holding a
    // lock. If the fallback ever runs, this says so before the hang would.
    api::log_info!(
        "[wifi] cache: {} parks, fell_back={}, last_state={:#x}",
        esp32_flash::PARKS.load(core::sync::atomic::Ordering::Relaxed),
        esp32_flash::PARK_FELL_BACK.load(core::sync::atomic::Ordering::Relaxed),
        esp32_flash::LAST_CACHE_STATE.load(core::sync::atomic::Ordering::Relaxed)
    );

    // The Wi-Fi power domain, before anything touches the radio.
    //
    // esp-idf clears `RTC_CNTL_WIFI_FORCE_PD` and `RTC_CNTL_WIFI_FORCE_ISO` in
    // `esp_wifi_bt_power_domain_on` (`phy_init.c:280-291`), called as the first
    // statement of `esp_wifi_init` (`wifi_init.c:182`) -- the wrapper this tree
    // bypasses by calling `esp_wifi_init_internal` directly. Both bits reset to
    // 0, so this may well be a no-op; the bootloader is what could have set
    // them, and that is a question to read rather than answer from the header.
    {
        const RTC_BASE: usize = 0x3FF4_8000;
        const DIG_PWC: usize = RTC_BASE + 0x84;
        const DIG_ISO: usize = RTC_BASE + 0x88;
        const WIFI_FORCE_PD: u32 = 1 << 17;
        const WIFI_FORCE_ISO: u32 = 1 << 28;
        let pwc = unsafe { (DIG_PWC as *const u32).read_volatile() };
        let iso = unsafe { (DIG_ISO as *const u32).read_volatile() };
        api::log_info!(
            "[wifi] rtc pwc={:#010x} iso={:#010x} force_pd={} force_iso={}",
            pwc,
            iso,
            pwc & WIFI_FORCE_PD != 0,
            iso & WIFI_FORCE_ISO != 0
        );
    }

    // Installed before init, not after. The driver posts `WIFI_READY` from
    // inside `esp_wifi_start`, and a handler registered afterwards would miss
    // the events that say the thing it is waiting for already happened.
    radio_esp32::events::set_handler(Some(on_event));

    radio_esp32::tasks::set_start_delay_ms(WORKER_START_DELAY_MS);
    if CONTROL_DELAY_MS > 0 {
        task::sleep_ms(CONTROL_DELAY_MS);
    }
    if WORKER_START_DELAY_MS > 0 || CONTROL_DELAY_MS > 0 {
        api::log_warn!(
            "[wifi] fault injection: worker delay {} ms, control delay {} ms",
            WORKER_START_DELAY_MS,
            CONTROL_DELAY_MS
        );
    }

    // A hang inside `init` prints nothing, so the question "is the whole
    // system dead, or is this one task stuck?" cannot be answered from the
    // silence. This task answers it: it is above this one's priority, so if
    // the tick still fires and the scheduler still runs, it wakes and reports
    // how far `init` got. Silence from it too means the system is gone, not
    // blocked. See doc/plan-radio.md, "N2".
    RUN_TASK.store(kernel::dynobj::current_task(), core::sync::atomic::Ordering::SeqCst);
    task::spawn("wifiwatch", watchdog, Priority::Normal(3), 4096);

    STAGE.store(1, core::sync::atomic::Ordering::SeqCst);
    let rc = unsafe { wifi::init() };
    STAGE.store(2, core::sync::atomic::Ordering::SeqCst);
    if rc != 0 {
        api::log_error!("[wifi] esp_wifi_init_internal failed: {:#x}", rc);
        return idle();
    }
    api::log_info!("[wifi] driver up");
    osi_calls("init");
    task_report("init");
    alloc_report("init");
    radio_esp32::adapter::set_alloc_phase(1);

    // NULL, start, *then* STA — Zephyr's order, not the obvious one.
    // `esp32_wifi_dev_init` calls `esp_wifi_init`, then
    // `esp_wifi_set_mode(ESP32_WIFI_MODE_NULL)`, then starts, and only moves to
    // STA when something asks it to (`drivers/wifi/esp32/src/esp_wifi_drv.c`,
    // lines 1854-1867). Going straight to STA before the start is what this
    // did, and the difference is what state the MAC is brought up in.
    let rc = unsafe { wifi::set_mode(wifi::mode::NULL) };
    if rc != 0 {
        api::log_error!("[wifi] set_mode(NULL) failed: {:#x}", rc);
        return idle();
    }

    let rc = unsafe { wifi::start() };
    if rc != 0 {
        api::log_error!("[wifi] esp_wifi_start failed: {:#x}", rc);
        return idle();
    }

    let rc = unsafe { wifi::set_mode(wifi::mode::STA) };
    if rc != 0 {
        api::log_error!("[wifi] set_mode(STA) failed: {:#x}", rc);
        return idle();
    }
    api::log_info!("[wifi] station started");
    osi_calls("start");
    task_report("start");
    alloc_report("start");
    radio_esp32::adapter::set_alloc_phase(2);

    // What the driver actually asked for. Recorded by `_set_intr` rather than
    // logged from inside it: see `radio_esp32::interrupts::for_each_route`.
    let mut routes = 0;
    radio_esp32::interrupts::for_each_route(|r| {
        routes += 1;
        api::log_info!(
            "[wifi] irq: source {} -> cpu-int {} on core {} ({})",
            r.source,
            r.num,
            r.core,
            if r.connected { "connected" } else { "REFUSED" }
        );
    });
    if routes == 0 {
        api::log_warn!("[wifi] the driver routed no interrupts; nothing will arrive");
    }
    api::log_info!(
        "[wifi] intenable={:#010x}",
        unsafe { kernel::arch::registers::read_intenable() }
    );

    // Scans, forever, so the output can be watched while networks come and go
    // — and so a driver that works once and wedges on the second attempt is
    // visible, which a single scan would hide.
    let mut round = 1u32;
    loop {
        scan_once(round);
        round += 1;
        task::sleep_ms(5000);
    }
}

#[cfg(feature = "blobs")]
fn scan_once(round: u32) {
    use core::sync::atomic::Ordering;
    use radio_esp32::scan;

    SCAN_DONE.store(false, Ordering::SeqCst);

    let config = scan::ScanConfig::default();
    let t0 = kernel::clock::now_us();
    // **Non-blocking**, as Zephyr does. The results are collected by the
    // handler when `SCAN_DONE` arrives; this task only asks and goes back to
    // waiting. A blocking scan would sit inside the driver for the whole 2.1
    // seconds and then read the results from the wrong task.
    let rc = unsafe { scan::start(&config, false) };
    if rc != 0 {
        api::log_error!("[wifi] scan {} refused: {:#x}", round, rc);
        return;
    }
    api::log_info!(
        "[wifi] scan {} started ({} refused mutex unlocks so far)",
        round,
        radio_esp32::adapter::mutex_unlock_failures()
    );

    // Long enough for all thirteen channels at the driver's default dwell,
    // plus slack. Reported if it passes without the event, because a scan that
    // never completes and a scan whose completion is not delivered look the
    // same from here and are different bugs.
    // The MAC's own interrupt registers, sampled alongside the CPU's.
    //
    // `WMAC_INT_RAW` is what the MAC sets when it has something to say;
    // `WMAC_INT_ENA` is the MAC's own mask, which is *inside* the block and
    // has nothing to do with the crossbar or `INTENABLE`. Three outcomes and
    // they point in three different directions:
    //
    //   ENA zero          — the blob never armed the MAC's receive interrupts.
    //   RAW bits, no CPU  — frames arrive; the loss is between MAC and CPU.
    //   both zero         — nothing is being received at all.
    //
    // Reads only. Nothing here writes a MAC register, so it cannot be what
    // `coex_bt_high_prio` was.
    const WMAC_BASE: usize = 0x6003_3000;
    const WMAC_INT_RAW: usize = WMAC_BASE + 0x0004;
    const WMAC_INT_ENA: usize = WMAC_BASE + 0x0010;
    let mut mac_raw_seen: u32 = 0;
    let mut mac_ena_seen: u32 = 0;

    let mut raw_seen: u32 = 0;
    let deadline = t0 + 6_000_000;
    while kernel::clock::now_us() < deadline {
        if SCAN_DONE.load(Ordering::SeqCst) {
            api::log_info!(
                "[wifi] raw INTERRUPT {:#010x} core {} crossbar[src0]={:?}",
                raw_seen,
                kernel::smp::current_core().0,
                unsafe { soc_esp32::intr_map::routed_to(0) }
            );
            api::log_info!(
                "[wifi] wmac raw {:#010x} ena {:#010x}",
                mac_raw_seen,
                mac_ena_seen
            );
            api::log_info!(
                "[wifi] scan {} done in {} ms, {} events, {} dropped, {} bytes free",
                round,
                (kernel::clock::now_us() - t0) / 1000,
                EVENTS.load(Ordering::Relaxed),
                radio_esp32::events::dropped(),
                kernel::heap::free_bytes(kernel::heap::Caps::Internal)
            );
            return;
        }
        // Sample the raw INTERRUPT register, which is what the crossbar sets
        // *before* INTENABLE decides whether the CPU takes it. Bit 0 asserting
        // here with `fires(0)` still zero would mean the source arrives and
        // the dispatch path drops it; never asserting means the crossbar or
        // the MAC, not us.
        raw_seen |= unsafe { kernel::arch::registers::read_interrupt() };
        mac_raw_seen |= unsafe { (WMAC_INT_RAW as *const u32).read_volatile() };
        mac_ena_seen |= unsafe { (WMAC_INT_ENA as *const u32).read_volatile() };
        task::sleep_ms(1);
    }
    api::log_error!("[wifi] scan {} produced no SCAN_DONE within 6 s", round);
}

/// Stop, but stay alive so the console keeps working and the log above stays
/// readable. A panic here would reset the board and take the reason with it.
#[cfg(feature = "blobs")]
fn idle() {
    loop {
        task::sleep_ms(1000);
    }
}
