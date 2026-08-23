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
//! # Bring-up
//!
//! The nvs/heap/init/mode/start dance lives in
//! [`radio_esp32::station::EspStation::bring_up`] now — this scans through the
//! generalized [`Station`] interface rather than the raw `esp_wifi_*` calls it
//! used to step through by hand. The elaborate hang-hunting instrumentation
//! this file once carried was for the start-up race of #67, since resolved;
//! `radioprobe` keeps the deliberate step-through for probing the sequence.
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
//! [`Station`]: hal::wifi::Station

#![no_std]
#![no_main]

use api::task;
use hal::types::Priority;

kernel::flint_app!(main, abi = 2);

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

/// Set when the scan completes, so `run` can pace itself.
#[cfg(feature = "blobs")]
static SCAN_DONE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The station's events, on the dispatch task.
///
/// Runs on `radio-event`, on its own stack, with nothing of the driver's held
/// — so it may call back into `esp_wifi_*`, and does: the scan results belong
/// to whoever handled `ScanDone`, which is what Zephyr does and what the
/// driver expects. Reading them from another task after the fact is a pattern
/// neither reference uses.
#[cfg(feature = "blobs")]
fn on_event(event: hal::wifi::StationEvent) {
    use hal::wifi::StationEvent;
    if let StationEvent::ScanDone { count } = event {
        report_results(count as usize);
        SCAN_DONE.store(true, core::sync::atomic::Ordering::SeqCst);
    }
}

/// Read the scan out and print it. **Called from the event handler.**
#[cfg(feature = "blobs")]
fn report_results(found: usize) {
    use radio_esp32::scan;

    // Zeroed rather than `MaybeUninit`: 80 bytes times 24 is under 2 KiB
    // against the event task's 8 KiB, and the driver writes every field it
    // reports. The cost is one memset per scan against a class of bug that
    // only appears when it returns fewer records than it said it would.
    let mut records = [scan::ApRecord::ZEROED; MAX_RESULTS];

    let shown = match unsafe { scan::ap_records(&mut records) } {
        Ok(n) => n as usize,
        Err(e) => {
            api::log_error!("[wifi] ap_records failed: {:#x}", e);
            return;
        }
    };

    if found > shown {
        api::log_warn!(
            "[wifi] {} networks, showing {} (raise MAX_RESULTS)",
            found,
            shown
        );
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
    }

    for r in &records[..shown] {
        let b = r.bssid;
        // Flint registers only the discovery-safe subset of IDF's supplicant
        // callbacks. Until the real IE parser lands, the blob leaves every
        // auth mode at zero; calling that "open" would be actively misleading.
        let auth = "unparsed";
        // Printed as a byte count when the SSID is not UTF-8, which is legal
        // 802.11 and does happen.
        match r.ssid_str() {
            Some("") => api::log_info!(
                "[wifi]   {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  ch{:<3} {:>4} dBm  {:<9} <hidden>",
                b[0], b[1], b[2], b[3], b[4], b[5],
                r.primary, r.rssi, auth
            ),
            Some(name) => api::log_info!(
                "[wifi]   {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  ch{:<3} {:>4} dBm  {:<9} {}",
                b[0], b[1], b[2], b[3], b[4], b[5],
                r.primary, r.rssi, auth, name
            ),
            None => api::log_info!(
                "[wifi]   {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  ch{:<3} {:>4} dBm  {:<9} <{} non-utf8 bytes>",
                b[0], b[1], b[2], b[3], b[4], b[5],
                r.primary, r.rssi, auth, r.ssid_bytes().len()
            ),
        }
    }
}

#[cfg(feature = "blobs")]
fn run() {
    use hal::wifi::{ScanRequest, Station};

    task::sleep_ms(200);
    api::log_info!("[wifi] starting");

    // The whole bring-up — nvs, heap, init, station mode, start — with the
    // event handler installed before the start. `radioprobe` still steps
    // through it by hand; a plain scan does not need to.
    let mut station = match radio_esp32::station::EspStation::bring_up(on_event) {
        Ok(s) => s,
        Err(e) => {
            api::log_error!("[wifi] bring-up failed: {}", e);
            task::exit();
        }
    };
    api::log_info!("[wifi] station started");

    // What the driver actually routed. Recorded by `_set_intr` rather than
    // logged from inside it; see `radio_esp32::interrupts::for_each_route`.
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

    // Scans forever, so the output can be watched while networks come and go
    // — and so a driver that works once and wedges on the second attempt is
    // visible, which a single scan would hide.
    let mut round = 1u32;
    loop {
        SCAN_DONE.store(false, core::sync::atomic::Ordering::SeqCst);
        match station.scan(&ScanRequest::default()) {
            Ok(()) => api::log_info!("[wifi] scan {} started", round),
            Err(e) => api::log_error!("[wifi] scan {} refused: {}", round, e),
        }
        round += 1;
        task::sleep_ms(5000);
    }
}
