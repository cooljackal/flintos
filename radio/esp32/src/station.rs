// SPDX-License-Identifier: Apache-2.0

//! The ESP32 blob backend of [`hal::wifi::Station`].
//!
//! This is phase 2 of the generalized Wi-Fi plan: the opt-in ("unsafe") radio
//! backend, built on Espressif's binary driver. An application holds a
//! `&mut dyn hal::wifi::Station` and this answers it — scan, connect,
//! disconnect, status — by translating to the blob's `esp_wifi_*` calls and
//! translating its events back.
//!
//! # What works without a supplicant, and what does not
//!
//! Associating to an **open** network needs no key handshake: set the config,
//! call `esp_wifi_connect`, and the driver posts `STA_CONNECTED`. That path is
//! complete here.
//!
//! **WPA2/WPA3** needs the 4-way handshake, which is not in the blobs — see
//! [`crate::wifi`]'s `WpaCallbacks`, currently discovery-only stubs. Until the
//! supplicant is wired into that seam, a WPA2 connect associates and then times
//! out, surfacing honestly as [`DisconnectReason::FourWayTimeout`]. The config
//! and the connect are built correctly here regardless; only the handshake is
//! missing, and it plugs into the existing `wpa_funcs` table.
//!
//! # The dangerous struct
//!
//! [`StaConfig`] mirrors `wifi_sta_config_t`, which the blob reads by fixed
//! offset. As with [`crate::wifi::WifiInitConfig`], the layout is asserted
//! field by field against the v4.4 header rather than trusted — a field at the
//! wrong offset does not fail to compile and does not fault, it silently
//! configures the radio wrong.

use core::ffi::c_char;
use core::sync::atomic::{AtomicU8, Ordering};

use hal::wifi::{
    ApInfo, Bssid, ConnectRequest, Credentials, DisconnectReason, EventHandler, ScanRequest,
    Security, Ssid, Station, StationEvent, StationState, StationStatus, WifiError, WifiResult,
};

use crate::wifi;

// ── wifi_sta_config_t, layout-asserted ───────────────────────────────────────

/// `wifi_scan_threshold_t`: the weakest AP a scan-driven connect will accept.
#[repr(C)]
#[derive(Clone, Copy)]
struct ScanThreshold {
    /// Minimum RSSI, dBm.
    rssi: i8,
    /// Minimum auth mode (a `wifi_auth_mode_t`). C pads three bytes before it.
    authmode: u32,
}

/// `wifi_pmf_config_t`: Protected Management Frames.
#[repr(C)]
#[derive(Clone, Copy)]
struct PmfConfig {
    capable: bool,
    required: bool,
}

/// `wifi_sta_config_t` — the station's target network.
///
/// 132 bytes at v4.4. The trailing `caps` word holds the `rm_enabled`,
/// `btm_enabled`, `mbo_enabled` bitfield; left zero, which is the default.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StaConfig {
    ssid: [u8; 32],
    password: [u8; 64],
    scan_method: u32,
    bssid_set: bool,
    bssid: [u8; 6],
    channel: u8,
    listen_interval: u16,
    sort_method: u32,
    threshold: ScanThreshold,
    pmf_cfg: PmfConfig,
    /// The `rm/btm/mbo_enabled` bitfield word, unused here.
    caps: u32,
}

// The v4.4 layout, asserted rather than trusted. Numbers are from
// `wifi_sta_config_t` in `esp_wifi_types.h`.
#[cfg(target_pointer_width = "32")]
const _: () = {
    use core::mem::offset_of;
    assert!(core::mem::size_of::<StaConfig>() == 132);
    assert!(offset_of!(StaConfig, password) == 32);
    assert!(offset_of!(StaConfig, scan_method) == 96);
    assert!(offset_of!(StaConfig, bssid_set) == 100);
    assert!(offset_of!(StaConfig, bssid) == 101);
    assert!(offset_of!(StaConfig, channel) == 107);
    assert!(offset_of!(StaConfig, listen_interval) == 108);
    assert!(offset_of!(StaConfig, sort_method) == 112);
    assert!(offset_of!(StaConfig, threshold) == 116);
    assert!(offset_of!(StaConfig, pmf_cfg) == 124);
    assert!(offset_of!(StaConfig, caps) == 128);
    // The nested threshold's own padding.
    assert!(core::mem::size_of::<ScanThreshold>() == 8);
    assert!(offset_of!(ScanThreshold, authmode) == 4);
};

/// `wifi_auth_mode_t` values used as a connect threshold. From
/// `esp_wifi_types.h`.
mod auth {
    pub const OPEN: u32 = 0;
    // Named for completeness and used in the tests; the connect threshold is
    // OPEN now (see `from_request`), so non-test code no longer reads it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const WPA2_PSK: u32 = 3;
    pub const WPA3_PSK: u32 = 6;
    pub const WPA2_WPA3_PSK: u32 = 7;
}

/// `WIFI_IF_STA`.
const IF_STA: u32 = 0;
/// `WIFI_ALL_CHANNEL_SCAN` — survey every channel for the best AP, rather than
/// stopping at the first match. The safe default for a connect.
const ALL_CHANNEL_SCAN: u32 = 1;

impl StaConfig {
    /// Build the driver config from a generalized [`ConnectRequest`].
    ///
    /// Returns [`WifiError::BadCredentials`] if the credentials do not fit the
    /// security, before the radio is touched — the same check both backends
    /// make.
    fn from_request(req: &ConnectRequest) -> WifiResult<Self> {
        if !req.credentials.valid_for(req.security) {
            return Err(WifiError::BadCredentials);
        }

        let mut cfg = StaConfig {
            ssid: [0; 32],
            password: [0; 64],
            scan_method: ALL_CHANNEL_SCAN,
            bssid_set: req.bssid.is_some(),
            bssid: req.bssid.unwrap_or([0; 6]),
            channel: req.channel.unwrap_or(0),
            listen_interval: 0,
            sort_method: 0, // by signal
            threshold: ScanThreshold {
                rssi: -127, // accept any signal
                // No security floor. Setting one to the requested mode filters
                // the connect scan, and on a WPA2/WPA3-transitional AP that
                // advertises a mode "above" the request it removes the very AP
                // we want and the driver reports NoApFound. Security is enforced
                // by the passphrase and the handshake, not by this filter; the
                // reference leaves it open too.
                authmode: auth::OPEN,
            },
            pmf_cfg: PmfConfig {
                // Declare the capability we actually have. This supplicant does
                // WPA2-PSK only — no PMF/BIP, no SAE. WPA3-SAE *requires* PMF, so
                // advertising PMF-capable on a WPA2/WPA3-transitional AP invites
                // the blob to negotiate SAE, whose authentication it cannot
                // complete (the wpa3_* callbacks are unset) — 802.11 auth then
                // expires (disconnect reason 2) before association. Not capable
                // for PSK keeps the connection on the WPA2 path; only the (as yet
                // unbuilt) SAE path asks for PMF.
                capable: matches!(req.security, Security::Wpa3Sae),
                required: matches!(req.security, Security::Wpa3Sae),
            },
            caps: 0,
        };

        let ssid = req.ssid.as_bytes();
        cfg.ssid[..ssid.len()].copy_from_slice(ssid);

        match req.credentials {
            Credentials::None => {}
            Credentials::Passphrase(p) => {
                cfg.password[..p.len()].copy_from_slice(p);
            }
            // esp-idf's config takes a passphrase or a 64-hex-digit PSK string,
            // not a raw 32-byte key. Mapping a raw PMK onto the blob path is
            // not supported; the pure-Rust backend uses it directly.
            Credentials::Psk(_) => return Err(WifiError::BadCredentials),
        }

        Ok(cfg)
    }
}

// ── The events the driver posts, layout-asserted ─────────────────────────────

/// `wifi_event_sta_connected_t`.
#[repr(C)]
struct StaConnected {
    ssid: [u8; 32],
    ssid_len: u8,
    bssid: [u8; 6],
    channel: u8,
    authmode: u32,
}

/// `wifi_event_sta_disconnected_t`.
#[repr(C)]
struct StaDisconnected {
    ssid: [u8; 32],
    ssid_len: u8,
    bssid: [u8; 6],
    reason: u8,
}

#[cfg(target_pointer_width = "32")]
const _: () = {
    assert!(core::mem::size_of::<StaConnected>() == 44);
    assert!(core::mem::size_of::<StaDisconnected>() == 40);
};

/// Map the driver's `wifi_err_reason_t` onto the generalized
/// [`DisconnectReason`]. Only the codes an application acts on are
/// distinguished; the rest collapse to `Unspecified`.
fn reason_from_code(code: u8) -> DisconnectReason {
    match code {
        15 => DisconnectReason::FourWayTimeout,       // 4WAY_HANDSHAKE_TIMEOUT
        201 => DisconnectReason::NoApFound,           // NO_AP_FOUND
        202 => DisconnectReason::AuthFailed,          // AUTH_FAIL
        203 => DisconnectReason::AssocFailed,         // ASSOC_FAIL
        204 => DisconnectReason::HandshakeTimeout,    // HANDSHAKE_TIMEOUT
        8 => DisconnectReason::Requested,             // ASSOC_LEAVE (we asked)
        // 2 AUTH_EXPIRE, 4 DISASSOC_DUE_TO_INACTIVITY, 200 BEACON_TIMEOUT, ...
        2 | 4 | 200 => DisconnectReason::ConnectionLost,
        _ => DisconnectReason::Unspecified,
    }
}

// ── State, shared with the event bridge ──────────────────────────────────────

// The lifecycle state, as a byte so the event bridge (which runs on the
// dispatch task) can update it without a lock. 0/1/2 = the StationState
// variants; see `state_from_byte`.
static STATE: AtomicU8 = AtomicU8::new(0);
// The last channel and rssi, for `status`. Small enough to be plain atomics.
static CHANNEL: AtomicU8 = AtomicU8::new(0);

const ST_DISCONNECTED: u8 = 0;
const ST_CONNECTING: u8 = 1;
const ST_CONNECTED: u8 = 2;

fn state_from_byte(b: u8) -> StationState {
    match b {
        ST_CONNECTING => StationState::Connecting,
        ST_CONNECTED => StationState::Connected,
        _ => StationState::Disconnected,
    }
}

// The application's handler, called by the bridge below. A plain function
// pointer in a static, matching the constraint that it runs on the dispatch
// task and carries no environment.
static APP_HANDLER: AtomicHandler = AtomicHandler::new();

/// A nullable `fn(StationEvent)` in a static, stored as its address.
struct AtomicHandler(core::sync::atomic::AtomicUsize);
impl AtomicHandler {
    const fn new() -> Self {
        Self(core::sync::atomic::AtomicUsize::new(0))
    }
    fn store(&self, h: Option<EventHandler>) -> Option<EventHandler> {
        let prev = self.0.swap(h.map_or(0, |f| f as usize), Ordering::SeqCst);
        (prev != 0).then(|| unsafe { core::mem::transmute::<usize, EventHandler>(prev) })
    }
    fn load(&self) -> Option<EventHandler> {
        let p = self.0.load(Ordering::SeqCst);
        (p != 0).then(|| unsafe { core::mem::transmute::<usize, EventHandler>(p) })
    }
}

/// The bridge installed with `events::set_handler`: translates a raw driver
/// event into a [`StationEvent`], updates the shared state, and calls the
/// application's handler. Runs on the event dispatch task.
fn bridge(base: *const c_char, id: i32, data: *mut core::ffi::c_void, len: usize) {
    // Only Wi-Fi events; IP events share the dispatch but a different base.
    if !core::ptr::eq(base, wifi::WIFI_EVENT.0) {
        return;
    }
    // Diagnostic: the raw event id, so the 802.11 progression (does association
    // ever complete?) is visible without inferring it from the mapped events.
    api::log_info!("[wifi] event id={} len={}", id, len);

    let event = match id {
        wifi::event::SCAN_DONE => {
            let count = unsafe { crate::scan::ap_count() }.unwrap_or(0);
            Some(StationEvent::ScanDone { count })
        }
        wifi::event::STA_CONNECTED if len >= core::mem::size_of::<StaConnected>() => {
            let e = unsafe { &*(data as *const StaConnected) };
            STATE.store(ST_CONNECTED, Ordering::SeqCst);
            CHANNEL.store(e.channel, Ordering::SeqCst);
            Some(StationEvent::Connected {
                bssid: e.bssid,
                channel: e.channel,
            })
        }
        wifi::event::STA_DISCONNECTED if len >= core::mem::size_of::<StaDisconnected>() => {
            let e = unsafe { &*(data as *const StaDisconnected) };
            STATE.store(ST_DISCONNECTED, Ordering::SeqCst);
            // Diagnostic: the raw reason code, finer than the mapped variant.
            api::log_warn!("[wifi] disconnect reason code {}", e.reason);
            Some(StationEvent::Disconnected {
                reason: reason_from_code(e.reason),
            })
        }
        _ => None,
    };

    if let (Some(ev), Some(handler)) = (event, APP_HANDLER.load()) {
        handler(ev);
    }
}

// ── The backend ──────────────────────────────────────────────────────────────

/// The ESP32 station. Zero-sized: all its state is the driver's, reached
/// through the `esp_wifi_*` calls, plus the shared statics above.
///
/// One exists per system — the radio is a singleton — so this is a handle, not
/// an owner. [`init`](Self::init) must have brought the driver up first.
#[derive(Clone, Copy)]
pub struct EspStation {
    _private: (),
}

impl EspStation {
    /// A handle to the station. The caller is asserting the driver has been
    /// initialised and started in station mode.
    ///
    /// # Safety
    /// `wifi::init`, `set_mode(STA)` and `start` must have succeeded, and only
    /// one live handle should drive the radio at a time.
    pub unsafe fn new() -> Self {
        Self { _private: () }
    }
}

impl Station for EspStation {
    fn scan(&mut self, request: &ScanRequest) -> WifiResult<()> {
        // A targeted SSID would set `ssid`; the generalized broadcast scan
        // leaves it null, which is the common case.
        let cfg = crate::scan::ScanConfig {
            channel: request.channel.unwrap_or(0),
            scan_type: if request.passive { 1 } else { 0 },
            ..Default::default()
        };
        let rc = unsafe { crate::scan::start(&cfg, false) };
        backend_result(rc)
    }

    fn scan_results(&mut self, out: &mut [ApInfo]) -> WifiResult<usize> {
        // Read into the driver's record type, then map to the generalized one.
        let mut records = [crate::scan::ApRecord::ZEROED; 24];
        let take = out.len().min(records.len());
        let got = unsafe { crate::scan::ap_records(&mut records[..take]) }
            .map_err(WifiError::Backend)? as usize;
        for (dst, src) in out.iter_mut().zip(records[..got].iter()) {
            *dst = ap_info_from(src);
        }
        Ok(got.min(out.len()))
    }

    fn connect(&mut self, request: &ConnectRequest) -> WifiResult<()> {
        let cfg = StaConfig::from_request(request)?;
        // Stage the credentials for the supplicant: when the driver calls back
        // into `sta_connect` it derives the PMK from these. Only a passphrase
        // needs staging; an open network runs no handshake.
        if let Credentials::Passphrase(p) = request.credentials {
            crate::supplicant::stage_credentials(request.ssid.as_bytes(), p);
        }
        let rc = unsafe { wifi::esp_wifi_set_config(IF_STA, &cfg) };
        backend_result(rc)?;
        STATE.store(ST_CONNECTING, Ordering::SeqCst);
        let rc = unsafe { wifi::esp_wifi_connect() };
        if rc != 0 {
            STATE.store(ST_DISCONNECTED, Ordering::SeqCst);
        }
        backend_result(rc)
    }

    fn disconnect(&mut self) -> WifiResult<()> {
        backend_result(unsafe { wifi::esp_wifi_disconnect() })
    }

    fn status(&self) -> StationStatus {
        StationStatus {
            state: state_from_byte(STATE.load(Ordering::SeqCst)),
            ssid: None,
            bssid: None,
            channel: CHANNEL.load(Ordering::SeqCst),
            rssi: 0,
            security: Security::Open,
        }
    }

    fn set_event_handler(&mut self, handler: Option<EventHandler>) -> Option<EventHandler> {
        let prev = APP_HANDLER.store(handler);
        // Install (or, if clearing, leave) the bridge with the raw dispatch.
        crate::events::set_handler(Some(bridge));
        prev
    }
}

/// esp-idf `esp_err_t` to a [`WifiResult`]. `ESP_OK` is 0.
fn backend_result(rc: i32) -> WifiResult<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(WifiError::Backend(rc))
    }
}

/// One driver scan record as a generalized [`ApInfo`].
fn ap_info_from(r: &crate::scan::ApRecord) -> ApInfo {
    ApInfo {
        ssid: Ssid::new(r.ssid_bytes()).unwrap_or_else(|| Ssid::new(b"").unwrap()),
        bssid: r.bssid as Bssid,
        channel: r.primary,
        rssi: r.rssi,
        security: security_from_authmode(r.authmode),
    }
}

/// A driver `wifi_auth_mode_t` as a generalized [`Security`]. Modes the
/// generalized enum does not name (WEP, enterprise) map to their nearest
/// meaning for display; connecting to them is a separate, unsupported matter.
fn security_from_authmode(mode: u32) -> Security {
    match mode {
        auth::OPEN => Security::Open,
        auth::WPA3_PSK => Security::Wpa3Sae,
        auth::WPA2_WPA3_PSK => Security::Wpa2Wpa3Psk,
        _ => Security::Wpa2Psk, // WPA_PSK, WPA2_PSK, WPA_WPA2_PSK, and unknowns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_codes_map_to_the_actionable_distinctions() {
        assert_eq!(reason_from_code(202), DisconnectReason::AuthFailed);
        assert_eq!(reason_from_code(201), DisconnectReason::NoApFound);
        assert_eq!(reason_from_code(15), DisconnectReason::FourWayTimeout);
        assert_eq!(reason_from_code(8), DisconnectReason::Requested);
        assert_eq!(reason_from_code(99), DisconnectReason::Unspecified);
    }

    #[test]
    fn authmode_round_trips_through_security() {
        assert_eq!(security_from_authmode(auth::OPEN), Security::Open);
        assert_eq!(security_from_authmode(auth::WPA2_PSK), Security::Wpa2Psk);
        assert_eq!(security_from_authmode(auth::WPA3_PSK), Security::Wpa3Sae);
        assert_eq!(
            security_from_authmode(auth::WPA2_WPA3_PSK),
            Security::Wpa2Wpa3Psk
        );
    }

    #[test]
    fn open_needs_no_credentials_and_psk_is_rejected_on_the_blob_path() {
        let ssid = Ssid::new(b"net").unwrap();
        let open = ConnectRequest {
            ssid,
            security: Security::Open,
            credentials: Credentials::None,
            bssid: None,
            channel: None,
        };
        assert!(StaConfig::from_request(&open).is_ok());

        let psk = ConnectRequest {
            ssid,
            security: Security::Wpa2Psk,
            credentials: Credentials::Psk([0; 32]),
            bssid: None,
            channel: None,
        };
        assert_eq!(
            StaConfig::from_request(&psk).err(),
            Some(WifiError::BadCredentials)
        );

        let wrong = ConnectRequest {
            ssid,
            security: Security::Wpa2Psk,
            credentials: Credentials::Passphrase(b"short"),
            bssid: None,
            channel: None,
        };
        assert_eq!(
            StaConfig::from_request(&wrong).err(),
            Some(WifiError::BadCredentials)
        );
    }
}
