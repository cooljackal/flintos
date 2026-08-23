// SPDX-License-Identifier: Apache-2.0

//! Joins a WPA2 network with the first-party Rust supplicant.
//!
//! The proof that the whole phase-3 stack works end to end on hardware: the
//! generalized [`Station`] interface, the ESP32 blob backend, and the
//! first-party 4-way handshake in `lib/wpa` over `lib/crypto`. This associates
//! to a real access point and reports what happened.
//!
//! # Credentials
//!
//! The SSID and passphrase are read from the environment **at compile time**,
//! never committed:
//!
//! ```text
//! FLINT_WIFI_SSID=MyNetwork FLINT_WIFI_PASS=the-passphrase \
//!     make flash APP=wificonnect BOARD=board-esp32-devkitc EXTRA_FEATURES=blobs
//! ```
//!
//! Built without them, it scans and then says what to set — so a clone still
//! builds and a credential is never a build dependency.

#![no_std]
#![no_main]

use api::task;
use hal::types::Priority;

kernel::flint_app!(main, abi = 2);

fn main() {
    task::spawn("wificonnect", run, Priority::Normal(2), 16384);
}

/// The network to join, from the environment at build time. `None` if unset.
#[cfg(feature = "blobs")]
const SSID: Option<&str> = option_env!("FLINT_WIFI_SSID");
#[cfg(feature = "blobs")]
const PASS: Option<&str> = option_env!("FLINT_WIFI_PASS");

/// Set when the diagnostic scan finishes.
#[cfg(feature = "blobs")]
static SCAN_DONE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The build with no archives to call.
#[cfg(not(feature = "blobs"))]
fn run() {
    loop {
        api::log_warn!("[wifi] built without the blobs; nothing to connect with");
        api::log_warn!("[wifi]   make blobs");
        api::log_warn!("[wifi]   FLINT_WIFI_SSID=.. FLINT_WIFI_PASS=.. make flash APP=wificonnect BOARD=.. EXTRA_FEATURES=blobs");
        task::sleep_ms(5000);
    }
}

#[cfg(feature = "blobs")]
fn run() {
    use hal::wifi::{ConnectRequest, Credentials, Security, Ssid, Station};

    task::sleep_ms(200);
    api::log_info!("[wifi] starting");

    if !unsafe { radio_esp32::nvs::init() } {
        api::log_warn!("[wifi] nvs unavailable; calibrating in full each boot");
    }
    let heap = unsafe { kernel::heap::init_from_map() };
    api::log_info!("[wifi] heap: {} bytes", heap);

    // Bring the radio up in station mode, as wifiscan does.
    let mut station = unsafe { radio_esp32::station::EspStation::new() };
    station.set_event_handler(Some(on_event));

    let rc = unsafe { radio_esp32::wifi::init() };
    if rc != 0 {
        api::log_error!("[wifi] init failed: {:#x}", rc);
        return idle();
    }
    // NULL, start, then STA — the order the driver wants (see wifiscan).
    for (label, m) in [
        ("null", radio_esp32::wifi::mode::NULL),
        ("sta", radio_esp32::wifi::mode::STA),
    ] {
        if label == "sta" {
            let rc = unsafe { radio_esp32::wifi::start() };
            if rc != 0 {
                api::log_error!("[wifi] start failed: {:#x}", rc);
                return idle();
            }
        }
        let rc = unsafe { radio_esp32::wifi::set_mode(m) };
        if rc != 0 {
            api::log_error!("[wifi] set_mode({}) failed: {:#x}", label, rc);
            return idle();
        }
    }
    api::log_info!("[wifi] station up");

    // Scan first and print what the radio sees, so a failed connect can be
    // told apart from a network that simply is not visible — and so the exact
    // SSID bytes and security of the target are on the record.
    {
        let req = hal::wifi::ScanRequest::default();
        if station.scan(&req).is_ok() {
            let deadline = kernel::clock::now_us() + 6_000_000;
            while !SCAN_DONE.load(core::sync::atomic::Ordering::SeqCst)
                && kernel::clock::now_us() < deadline
            {
                task::sleep_ms(50);
            }
        }
    }

    let (Some(ssid), Some(pass)) = (SSID, PASS) else {
        api::log_warn!("[wifi] no credentials compiled in; set FLINT_WIFI_SSID / FLINT_WIFI_PASS");
        return idle();
    };
    let Some(ssid_val) = Ssid::new(ssid.as_bytes()) else {
        api::log_error!("[wifi] SSID too long");
        return idle();
    };

    api::log_info!("[wifi] connecting to {:?}", ssid);
    let req = ConnectRequest {
        ssid: ssid_val,
        security: Security::Wpa2Psk,
        credentials: Credentials::Passphrase(pass.as_bytes()),
        bssid: None,
        channel: None,
    };
    match station.connect(&req) {
        Ok(()) => api::log_info!("[wifi] connect accepted; awaiting handshake"),
        Err(e) => api::log_error!("[wifi] connect refused: {:?}", e),
    }

    // The result arrives at `on_event`. Report the running state each second.
    let mut i = 0;
    loop {
        task::sleep_ms(1000);
        i += 1;
        if i % 5 == 0 {
            api::log_info!("[wifi] state: {:?}", station.status().state);
        }
    }
}

/// The station's events, on the dispatch task.
#[cfg(feature = "blobs")]
fn on_event(event: hal::wifi::StationEvent) {
    use hal::wifi::StationEvent;
    match event {
        StationEvent::ScanDone { count } => {
            api::log_info!("[wifi] scan done: {} APs", count);
            // Print each SSID and its security, so the connect target can be
            // compared against exactly what the radio sees. Uses the raw scan
            // records for the auth-mode name.
            let mut records = [radio_esp32::scan::ApRecord::ZEROED; 24];
            if let Ok(n) = unsafe { radio_esp32::scan::ap_records(&mut records) } {
                for r in &records[..n as usize] {
                    match r.ssid_str() {
                        Some(s) => api::log_info!(
                            "[wifi]   \"{}\" ch{} {} dBm {}",
                            s,
                            r.primary,
                            r.rssi,
                            radio_esp32::scan::auth_name(r.authmode)
                        ),
                        None => api::log_info!(
                            "[wifi]   <{} non-utf8 bytes> ch{}",
                            r.ssid_bytes().len(),
                            r.primary
                        ),
                    }
                }
            }
            SCAN_DONE.store(true, core::sync::atomic::Ordering::SeqCst);
        }
        StationEvent::Connected { bssid, channel } => api::log_info!(
            "[wifi] CONNECTED to {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} on channel {}",
            bssid[0], bssid[1], bssid[2], bssid[3], bssid[4], bssid[5], channel
        ),
        StationEvent::Disconnected { reason } => {
            api::log_warn!("[wifi] DISCONNECTED: {:?}", reason)
        }
    }
}

#[cfg(feature = "blobs")]
fn idle() -> ! {
    loop {
        task::sleep_ms(5000);
    }
}
