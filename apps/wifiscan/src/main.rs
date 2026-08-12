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

    // Installed before init, not after. The driver posts `WIFI_READY` from
    // inside `esp_wifi_start`, and a handler registered afterwards would miss
    // the events that say the thing it is waiting for already happened.
    radio_esp32::events::set_handler(Some(on_event));

    let rc = unsafe { wifi::init() };
    if rc != 0 {
        api::log_error!("[wifi] esp_wifi_init_internal failed: {:#x}", rc);
        return idle();
    }
    api::log_info!("[wifi] driver up");

    let rc = unsafe { wifi::set_mode(wifi::mode::STA) };
    if rc != 0 {
        api::log_error!("[wifi] set_mode(STA) failed: {:#x}", rc);
        return idle();
    }

    let rc = unsafe { wifi::start() };
    if rc != 0 {
        api::log_error!("[wifi] esp_wifi_start failed: {:#x}", rc);
        return idle();
    }
    api::log_info!("[wifi] station started");

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
