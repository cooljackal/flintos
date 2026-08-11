// SPDX-License-Identifier: Apache-2.0

//! Scanning for access points. Step 5.2 of `doc/plan-radio.md`, issue #67.
//!
//! # The three structs, and why their offsets are asserted
//!
//! `wifi_scan_config_t` goes *into* the blob and `wifi_ap_record_t` comes back
//! out of it, so both are read by code nobody here can inspect. A field at the
//! wrong offset does not fail to compile and does not fault — it reads the
//! neighbouring field, and the symptom is an SSID that is one byte short or an
//! RSSI that is really a channel number.
//!
//! The layouts are transcribed from `esp_wifi_types.h` at the pinned v4.4 and
//! pinned by `const _: () = assert!(...)` on the target, where the compiler
//! knows the real offsets. The three that actually move are called out below:
//! the C enums are 4 bytes and force alignment, which is what puts `second` at
//! 40 rather than 39 and `authmode` at 48 rather than 45.
//!
//! # Who frees the results
//!
//! The driver allocates the result list — out of *our* heap, through the OSI
//! table — and holds it until [`ap_records`] is called, which copies it out and
//! frees it. Calling [`ap_count`] and then never calling [`ap_records`] leaks
//! it until the next scan. That is esp-idf's contract, not an artefact here,
//! and [`ap_records`] is written so the leak needs an early return to happen.

/// `wifi_country_t`. Sits at the tail of every [`ApRecord`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Country {
    /// Country code, NUL-padded rather than NUL-terminated in practice.
    pub cc: [u8; 3],
    /// First channel.
    pub schan: u8,
    /// How many channels from `schan`.
    pub nchan: u8,
    /// Maximum transmit power, in 0.25 dBm units.
    pub max_tx_power: i8,
    /// `wifi_country_policy_t`: 0 auto, 1 manual.
    pub policy: u32,
}

/// `wifi_ap_record_t`. One access point, as the driver saw it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ApRecord {
    pub bssid: [u8; 6],
    /// The SSID, NUL-terminated. 32 bytes plus the terminator.
    pub ssid: [u8; 33],
    /// Primary channel.
    pub primary: u8,
    /// `wifi_second_chan_t`: 0 none, 1 above, 2 below.
    pub second: u32,
    /// Signal strength, dBm. Negative.
    pub rssi: i8,
    /// `wifi_auth_mode_t`: 0 open, 3 WPA2-PSK, and so on. See [`auth_name`].
    pub authmode: u32,
    pub pairwise_cipher: u32,
    pub group_cipher: u32,
    /// `wifi_ant_t`.
    pub ant: u32,
    /// Seven one-bit flags and 25 reserved bits: 11b, 11g, 11n, LR, WPS, and
    /// the two FTM roles, in that order from bit 0.
    ///
    /// Kept as the raw word rather than unpacked into `bool`s. Rust has no
    /// bitfields, and a hand-written unpacking is one more thing that can
    /// disagree with the header while still compiling.
    pub flags: u32,
    pub country: Country,
}

impl ApRecord {
    /// The SSID as a string, stopping at the first NUL.
    ///
    /// Returns `None` for an SSID that is not UTF-8. That is legal in 802.11 —
    /// the field is opaque bytes — so a caller that wants to print something
    /// regardless should fall back to [`ApRecord::ssid_bytes`].
    pub fn ssid_str(&self) -> Option<&str> {
        core::str::from_utf8(self.ssid_bytes()).ok()
    }

    /// The SSID's bytes, up to the first NUL. Empty for a hidden network.
    pub fn ssid_bytes(&self) -> &[u8] {
        let end = self.ssid.iter().position(|&b| b == 0).unwrap_or(self.ssid.len());
        &self.ssid[..end]
    }

    /// Whether the network is open.
    pub fn is_open(&self) -> bool {
        self.authmode == AUTH_OPEN
    }
}

/// `wifi_auth_mode_t` values worth naming.
pub const AUTH_OPEN: u32 = 0;
pub const AUTH_WEP: u32 = 1;
pub const AUTH_WPA_PSK: u32 = 2;
pub const AUTH_WPA2_PSK: u32 = 3;
pub const AUTH_WPA_WPA2_PSK: u32 = 4;
pub const AUTH_WPA2_ENTERPRISE: u32 = 5;
pub const AUTH_WPA3_PSK: u32 = 6;
pub const AUTH_WPA2_WPA3_PSK: u32 = 7;
pub const AUTH_WAPI_PSK: u32 = 8;

/// A short name for an auth mode, for logging.
pub fn auth_name(mode: u32) -> &'static str {
    match mode {
        AUTH_OPEN => "open",
        AUTH_WEP => "WEP",
        AUTH_WPA_PSK => "WPA",
        AUTH_WPA2_PSK => "WPA2",
        AUTH_WPA_WPA2_PSK => "WPA/WPA2",
        AUTH_WPA2_ENTERPRISE => "WPA2-ent",
        AUTH_WPA3_PSK => "WPA3",
        AUTH_WPA2_WPA3_PSK => "WPA2/WPA3",
        AUTH_WAPI_PSK => "WAPI",
        _ => "?",
    }
}

/// `wifi_scan_time_t`'s active half.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ActiveScanTime {
    /// Minimum dwell per channel, ms.
    pub min: u32,
    /// Maximum dwell per channel, ms. **Above 1500 ms an associated station
    /// drops its AP**, which is Espressif's warning and not a guess.
    pub max: u32,
}

/// `wifi_scan_time_t`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScanTime {
    pub active: ActiveScanTime,
    /// Passive dwell per channel, ms. Same 1500 ms caveat.
    pub passive: u32,
}

/// `wifi_scan_config_t`.
#[repr(C)]
pub struct ScanConfig {
    /// Scan for one SSID only, or null for all of them.
    pub ssid: *mut u8,
    /// Scan for one BSSID only, or null.
    pub bssid: *mut u8,
    /// One channel, or 0 for all of them.
    pub channel: u8,
    /// Report networks that do not broadcast their SSID.
    pub show_hidden: bool,
    /// 0 active (probe requests), 1 passive (listen for beacons).
    pub scan_type: u32,
    pub scan_time: ScanTime,
}

/// Active scan of every channel.
///
/// The dwell times are esp-idf's own defaults — 0 means "the driver's
/// default", which is 120 ms maximum active — rather than numbers chosen here.
/// A scan of all thirteen channels therefore takes on the order of a second
/// and a half, which is worth knowing before deciding it has hung.
impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            ssid: core::ptr::null_mut(),
            bssid: core::ptr::null_mut(),
            channel: 0,
            show_hidden: false,
            scan_type: 0,
            scan_time: ScanTime {
                active: ActiveScanTime { min: 0, max: 0 },
                passive: 0,
            },
        }
    }
}

// The offsets the blob reads by. Target-only: on a 64-bit host the pointers in
// `ScanConfig` are wider and none of these hold, and the host build has no
// blob to disagree with anyway.
#[cfg(target_pointer_width = "32")]
mod layout {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    const _: () = assert!(size_of::<ScanConfig>() == 28);
    const _: () = assert!(offset_of!(ScanConfig, channel) == 8);
    const _: () = assert!(offset_of!(ScanConfig, show_hidden) == 9);
    // The first enum, and the first place padding appears: `show_hidden` is one
    // byte at 9 and `scan_type` is a 4-byte enum, so two bytes are skipped.
    const _: () = assert!(offset_of!(ScanConfig, scan_type) == 12);
    const _: () = assert!(offset_of!(ScanConfig, scan_time) == 16);

    const _: () = assert!(size_of::<Country>() == 12);
    const _: () = assert!(offset_of!(Country, policy) == 8);

    const _: () = assert!(size_of::<ApRecord>() == 80);
    const _: () = assert!(align_of::<ApRecord>() == 4);
    const _: () = assert!(offset_of!(ApRecord, ssid) == 6);
    const _: () = assert!(offset_of!(ApRecord, primary) == 39);
    // `primary` ends at 40 and `second` is a 4-byte enum, so this one happens
    // to need no padding -- the near miss is worth pinning.
    const _: () = assert!(offset_of!(ApRecord, second) == 40);
    const _: () = assert!(offset_of!(ApRecord, rssi) == 44);
    // Three bytes of padding: this is the one that moves if `rssi` is ever
    // widened, and it would take every field after it along.
    const _: () = assert!(offset_of!(ApRecord, authmode) == 48);
    const _: () = assert!(offset_of!(ApRecord, flags) == 64);
    const _: () = assert!(offset_of!(ApRecord, country) == 68);
}

extern "C" {
    fn esp_wifi_scan_start(config: *const ScanConfig, block: bool) -> i32;
    fn esp_wifi_scan_stop() -> i32;
    fn esp_wifi_scan_get_ap_num(number: *mut u16) -> i32;
    fn esp_wifi_scan_get_ap_records(number: *mut u16, records: *mut ApRecord) -> i32;
}

/// `esp_wifi_scan_start`.
///
/// With `block` set this does not return until the scan finishes, which for an
/// all-channel active scan is well over a second. The blocking form is what
/// makes a scan readable without an event loop; the non-blocking form needs
/// [`crate::wifi::event::SCAN_DONE`] through
/// [`crate::adapter::set_event_handler`].
///
/// **Blocking here blocks the calling task, not the Wi-Fi task.** The driver
/// waits on its own event group, which reaches `_event_group_wait_bits` in the
/// OSI table and blocks whoever called in — so an application task calling
/// this yields the CPU rather than spinning.
///
/// # Safety
/// Calls into the blob. [`crate::wifi::start`] must have succeeded.
pub unsafe fn start(config: &ScanConfig, block: bool) -> i32 {
    unsafe { esp_wifi_scan_start(config, block) }
}

/// `esp_wifi_scan_stop`.
///
/// # Safety
/// Calls into the blob.
pub unsafe fn stop() -> i32 {
    unsafe { esp_wifi_scan_stop() }
}

/// How many access points the last scan found.
///
/// # Safety
/// Calls into the blob.
pub unsafe fn ap_count() -> Result<u16, i32> {
    let mut n: u16 = 0;
    let rc = unsafe { esp_wifi_scan_get_ap_num(&mut n) };
    if rc == 0 {
        Ok(n)
    } else {
        Err(rc)
    }
}

/// Copy the last scan's results into `out`, and release the driver's copy.
///
/// Returns how many records were written, which is `min(found, out.len())`.
/// **The driver frees its list here whether or not `out` was big enough**, so
/// a short buffer loses the overflow rather than leaving it for a second call
/// — that is esp-idf's behaviour and the reason [`ap_count`] exists.
///
/// # Safety
/// Calls into the blob, which writes `out`.
pub unsafe fn ap_records(out: &mut [ApRecord]) -> Result<u16, i32> {
    if out.is_empty() {
        return Ok(0);
    }
    let mut n = out.len().min(u16::MAX as usize) as u16;
    let rc = unsafe { esp_wifi_scan_get_ap_records(&mut n, out.as_mut_ptr()) };
    if rc == 0 {
        Ok(n)
    } else {
        Err(rc)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A record with a known SSID, built field by field so the test does not
    /// depend on the layout it is not checking.
    fn record_with(ssid: &[u8], authmode: u32) -> ApRecord {
        let mut r = ApRecord {
            bssid: [0; 6],
            ssid: [0; 33],
            primary: 1,
            second: 0,
            rssi: -50,
            authmode,
            pairwise_cipher: 0,
            group_cipher: 0,
            ant: 0,
            flags: 0,
            country: Country { cc: *b"GB\0", schan: 1, nchan: 13, max_tx_power: 78, policy: 0 },
        };
        r.ssid[..ssid.len()].copy_from_slice(ssid);
        r
    }

    #[test]
    fn an_ssid_stops_at_the_nul_and_not_at_the_end_of_the_field() {
        // The field is 33 bytes and almost never full. Reading all 33 gives a
        // name with a tail of NULs, which prints as the right name and
        // compares as the wrong one.
        let r = record_with(b"flintnet", AUTH_WPA2_PSK);
        assert_eq!(r.ssid_bytes(), b"flintnet");
        assert_eq!(r.ssid_str(), Some("flintnet"));
        assert!(!r.is_open());
    }

    #[test]
    fn a_full_length_ssid_has_no_terminator_to_find() {
        // 32 bytes is the maximum, and the 33rd is the terminator. An SSID
        // that fills the field must not run into it or past it.
        let name = [b'x'; 32];
        let r = record_with(&name, AUTH_OPEN);
        assert_eq!(r.ssid_bytes().len(), 32);
        assert!(r.is_open());
    }

    #[test]
    fn a_hidden_network_reports_an_empty_ssid_rather_than_junk() {
        let r = record_with(b"", AUTH_OPEN);
        assert_eq!(r.ssid_bytes(), b"");
        assert_eq!(r.ssid_str(), Some(""));
    }

    #[test]
    fn a_non_utf8_ssid_is_refused_rather_than_mangled() {
        // Legal in 802.11: the SSID is opaque bytes. `ssid_str` says so
        // instead of producing replacement characters.
        let r = record_with(&[0xFF, 0xFE, 0x41], AUTH_OPEN);
        assert_eq!(r.ssid_str(), None);
        assert_eq!(r.ssid_bytes(), &[0xFF, 0xFE, 0x41]);
    }

    #[test]
    fn every_auth_mode_in_the_header_has_a_name() {
        // A scan that reports "?" for a common network is a scan that looks
        // broken. The enum is contiguous 0..=8 at v4.4.
        for m in 0..=8u32 {
            assert_ne!(auth_name(m), "?", "auth mode {m} has no name");
        }
        assert_eq!(auth_name(9), "?");
    }

    #[test]
    fn the_default_scan_is_active_on_every_channel() {
        let c = ScanConfig::default();
        assert_eq!(c.channel, 0, "0 means every channel");
        assert_eq!(c.scan_type, 0, "0 is active");
        assert!(c.ssid.is_null() && c.bssid.is_null());
        // Zero dwell means the driver's default, not an instant scan.
        assert_eq!(c.scan_time.active.max, 0);
    }
}
