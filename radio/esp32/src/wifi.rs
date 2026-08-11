// SPDX-License-Identifier: Apache-2.0

//! `esp_wifi_init_internal` — handing the Wi-Fi driver its configuration.
//!
//! Step 5.1. The first call into the Wi-Fi half of the blobs, as
//! [`crate::phy`] was the first into the RF half.
//!
//! # This struct is the dangerous one
//!
//! `doc/plan-radio.md` warns at step 3.4 that a wrong IDF version shows up
//! "as a magic-number mismatch if you are lucky, and as a working radio that
//! corrupts memory if you are not". [`WifiInitConfig`] is where that happens:
//! the blob reads 200 bytes at whatever layout it was compiled against, and
//! nothing checks the middle of it. `magic` is last precisely so that a
//! struct which is the wrong *size* fails the magic test — but a struct that
//! is the right size with a field in the wrong place passes.
//!
//! So the layout is asserted rather than described. The `const` blocks below
//! pin the size of both structs and the offset of every field whose position
//! is load-bearing, against the numbers in `esp_wifi.h` and
//! `esp_wifi_crypto_types.h` at v4.4. A change to either fails the build here
//! instead of on the air.
//!
//! # What is deliberately null
//!
//! [`WpaCryptoFuncs`] is all-null except its self-describing `size` and
//! `version`. esp-idf fills it from `libwpa_supplicant`, which is C source and
//! **not one of the blobs**, so every entry is something FlintOS would have to
//! provide: AES, SHA-1, SHA-256, HMAC, PBKDF2, RC4, CCMP, GMAC.
//!
//! That is WPA2's problem, not init's and not an open scan's — which is why
//! step 5.2 (scan) comes before 5.3 (associate) here rather than in issue
//! order. A null table is the honest encoding of "no crypto yet": the blob
//! calls through it only when it needs to authenticate, and a null pointer
//! there is a fault at a known address rather than a silently wrong key.

use core::ffi::c_void;

use crate::osi::WifiOsiFuncs;

/// `WIFI_INIT_CONFIG_MAGIC` from `esp_wifi.h`. Last field of the struct, so a
/// size mismatch lands somewhere else and fails this check.
pub const INIT_CONFIG_MAGIC: i32 = 0x1F2F_3F4F;

/// `wpa_crypto_funcs_t`. Twenty-five function pointers behind two words.
///
/// Embedded **by value** in [`WifiInitConfig`], not behind a pointer, so its
/// size is part of that struct's layout and every field after it moves if this
/// one is wrong.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct WpaCryptoFuncs {
    /// `sizeof(wpa_crypto_funcs_t)`. Self-describing, and the blob is entitled
    /// to check it.
    pub size: u32,
    pub version: u32,
    pub aes_wrap: Option<extern "C" fn()>,
    pub aes_unwrap: Option<extern "C" fn()>,
    pub hmac_sha256_vector: Option<extern "C" fn()>,
    pub sha256_prf: Option<extern "C" fn()>,
    pub hmac_md5: Option<extern "C" fn()>,
    pub hmac_md5_vector: Option<extern "C" fn()>,
    pub hmac_sha1: Option<extern "C" fn()>,
    pub hmac_sha1_vector: Option<extern "C" fn()>,
    pub sha1_prf: Option<extern "C" fn()>,
    pub sha1_vector: Option<extern "C" fn()>,
    pub pbkdf2_sha1: Option<extern "C" fn()>,
    pub rc4_skip: Option<extern "C" fn()>,
    pub md5_vector: Option<extern "C" fn()>,
    pub aes_encrypt: Option<extern "C" fn()>,
    pub aes_encrypt_init: Option<extern "C" fn()>,
    pub aes_encrypt_deinit: Option<extern "C" fn()>,
    pub aes_decrypt: Option<extern "C" fn()>,
    pub aes_decrypt_init: Option<extern "C" fn()>,
    pub aes_decrypt_deinit: Option<extern "C" fn()>,
    pub aes_128_encrypt: Option<extern "C" fn()>,
    pub aes_128_decrypt: Option<extern "C" fn()>,
    pub omac1_aes_128: Option<extern "C" fn()>,
    pub ccmp_decrypt: Option<extern "C" fn()>,
    pub ccmp_encrypt: Option<extern "C" fn()>,
    pub aes_gmac: Option<extern "C" fn()>,
}

/// The size `esp_wifi_crypto_types.h` gives it: two `uint32_t` and
/// twenty-five pointers.
pub const WPA_CRYPTO_FUNCS_LEN: usize = 8 + 25 * 4;

impl WpaCryptoFuncs {
    /// Every entry null, `size` and `version` filled in.
    ///
    /// `version` is 1: esp-idf's `g_wifi_default_wpa_crypto_funcs` sets
    /// `.version = 1` and nothing here has reason to claim otherwise.
    pub const fn empty() -> Self {
        Self {
            // Its own size, not a constant that could drift from it.
            size: core::mem::size_of::<Self>() as u32,
            version: 1,
            aes_wrap: None,
            aes_unwrap: None,
            hmac_sha256_vector: None,
            sha256_prf: None,
            hmac_md5: None,
            hmac_md5_vector: None,
            hmac_sha1: None,
            hmac_sha1_vector: None,
            sha1_prf: None,
            sha1_vector: None,
            pbkdf2_sha1: None,
            rc4_skip: None,
            md5_vector: None,
            aes_encrypt: None,
            aes_encrypt_init: None,
            aes_encrypt_deinit: None,
            aes_decrypt: None,
            aes_decrypt_init: None,
            aes_decrypt_deinit: None,
            aes_128_encrypt: None,
            aes_128_decrypt: None,
            omac1_aes_128: None,
            ccmp_decrypt: None,
            ccmp_encrypt: None,
            aes_gmac: None,
        }
    }
}

/// `wifi_init_config_t`, field for field and in order.
///
/// The order is the header's and may not be tidied. `magic` is last because
/// the header says so — "it should be the last field" — and that is the only
/// thing standing between a version mismatch and a radio that runs on
/// misaligned configuration.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct WifiInitConfig {
    /// `system_event_handler_t`. esp-idf passes `esp_event_send_internal`.
    pub event_handler: Option<extern "C" fn(*mut c_void) -> i32>,
    pub osi_funcs: *const WifiOsiFuncs,
    pub wpa_crypto_funcs: WpaCryptoFuncs,
    pub static_rx_buf_num: i32,
    pub dynamic_rx_buf_num: i32,
    pub tx_buf_type: i32,
    pub static_tx_buf_num: i32,
    pub dynamic_tx_buf_num: i32,
    pub cache_tx_buf_num: i32,
    pub csi_enable: i32,
    pub ampdu_rx_enable: i32,
    pub ampdu_tx_enable: i32,
    pub amsdu_tx_enable: i32,
    pub nvs_enable: i32,
    pub nano_enable: i32,
    pub rx_ba_win: i32,
    pub wifi_task_core_id: i32,
    pub beacon_max_len: i32,
    pub mgmt_sbuf_num: i32,
    pub feature_caps: u64,
    pub sta_disconnected_pm: bool,
    pub magic: i32,
}

/// `sizeof(wifi_init_config_t)`.
///
/// Two pointers, the crypto table, sixteen `int`s, then a `uint64_t` which
/// forces four bytes of padding before it, a `bool`, three more bytes of
/// padding, and the magic.
pub const INIT_CONFIG_LEN: usize = 200;

// The layout, asserted rather than trusted. Each of these is a number from
// `esp_wifi.h` at v4.4; a field inserted in the wrong place stops the build.
//
// **Thirty-two-bit only, and that is not a hedge.** These sizes are facts
// about the ESP32, where a pointer is four bytes. The same struct on a 64-bit
// host is a different size by construction, and asserting the ESP32's numbers
// there would fail for a reason that has nothing to do with being wrong. The
// host keeps the checks that are width-independent -- `size` describing
// itself, `magic` being last -- in the tests below.
#[cfg(target_pointer_width = "32")]
const _: () = {
    assert!(core::mem::size_of::<WpaCryptoFuncs>() == WPA_CRYPTO_FUNCS_LEN);
    assert!(core::mem::size_of::<WifiInitConfig>() == INIT_CONFIG_LEN);
    assert!(core::mem::offset_of!(WifiInitConfig, osi_funcs) == 4);
    assert!(core::mem::offset_of!(WifiInitConfig, wpa_crypto_funcs) == 8);
    assert!(core::mem::offset_of!(WifiInitConfig, static_rx_buf_num) == 116);
    // The two that padding decides, and therefore the two most likely to be
    // wrong: `feature_caps` is a `uint64_t` after an odd number of words, and
    // `magic` follows a `bool`.
    assert!(core::mem::offset_of!(WifiInitConfig, feature_caps) == 184);
    assert!(core::mem::offset_of!(WifiInitConfig, magic) == 196);
};

impl WifiInitConfig {
    /// esp-idf's `WIFI_INIT_CONFIG_DEFAULT()`, for a build with no SPIRAM and
    /// dynamic TX buffers — which is what FlintOS is.
    ///
    /// Every number is the Kconfig default at v4.4, and where a macro chooses
    /// between two, the branch this build takes is named:
    ///
    /// | Field | Value | Where from |
    /// |---|---|---|
    /// | `static_rx_buf_num` | 10 | `ESP32_WIFI_STATIC_RX_BUFFER_NUM`, no-SPIRAM branch |
    /// | `dynamic_rx_buf_num` | 32 | `ESP32_WIFI_DYNAMIC_RX_BUFFER_NUM` |
    /// | `tx_buf_type` | 1 | dynamic |
    /// | `static_tx_buf_num` | 0 | `WIFI_STATIC_TX_BUFFER_NUM` is 0 unless static TX is picked |
    /// | `dynamic_tx_buf_num` | 32 | `ESP32_WIFI_DYNAMIC_TX_BUFFER_NUM` |
    /// | `cache_tx_buf_num` | 0 | SPIRAM only |
    /// | `rx_ba_win` | 6 | `ESP32_WIFI_RX_BA_WIN`, no-SPIRAM branch |
    /// | `beacon_max_len` | 752 | `WIFI_SOFTAP_BEACON_MAX_LEN` |
    /// | `mgmt_sbuf_num` | 32 | `WIFI_MGMT_SBUF_NUM` |
    ///
    /// `nvs_enable` is 1, so the driver stores its country code and AP record
    /// through [`crate::nvs`]. Those shims exist and are host-tested; this is
    /// the first thing that will call them for real.
    ///
    /// `feature_caps` is 0: no WPA3, no cached TX buffers, no FTM. Each of
    /// those bits turns on a code path that would need something FlintOS has
    /// not got, and claiming a capability is how you get a blob calling into a
    /// null.
    pub const fn defaults(osi_funcs: *const WifiOsiFuncs) -> Self {
        Self {
            event_handler: Some(event_post),
            osi_funcs,
            wpa_crypto_funcs: WpaCryptoFuncs::empty(),
            static_rx_buf_num: 10,
            dynamic_rx_buf_num: 32,
            tx_buf_type: 1,
            static_tx_buf_num: 0,
            dynamic_tx_buf_num: 32,
            cache_tx_buf_num: 0,
            csi_enable: 0,
            ampdu_rx_enable: 1,
            ampdu_tx_enable: 1,
            amsdu_tx_enable: 0,
            nvs_enable: 1,
            nano_enable: 0,
            rx_ba_win: 6,
            wifi_task_core_id: 0,
            beacon_max_len: 752,
            mgmt_sbuf_num: 32,
            feature_caps: 0,
            sta_disconnected_pm: false,
            magic: INIT_CONFIG_MAGIC,
        }
    }
}

/// Where the driver reports what happened. **A stub, and step 5.2's problem.**
///
/// esp-idf points this at `esp_event_send_internal`, which posts into
/// `esp_event`'s loop; the driver announces `WIFI_EVENT_SCAN_DONE`,
/// `WIFI_EVENT_STA_START`, `WIFI_EVENT_STA_CONNECTED` and the rest through it.
/// FlintOS has no event loop, so this accepts and drops.
///
/// That is enough for init, which is why 5.1 can be done without it, and not
/// enough for a scan — a scan that completes and cannot say so is a scan you
/// cannot read. Returning `ESP_OK` rather than an error is deliberate: the
/// driver treats a failed post as a problem worth retrying, and there is
/// nothing here to retry into.
extern "C" fn event_post(_event: *mut c_void) -> i32 {
    0
}

extern "C" {
    /// `esp_err_t esp_wifi_init_internal(const wifi_init_config_t *config)`,
    /// from `esp_private/wifi.h` at v4.4. Defined in `libnet80211.a`.
    fn esp_wifi_init_internal(config: *const WifiInitConfig) -> i32;
}

/// Bring the Wi-Fi driver up.
///
/// Allocates the driver's control structures and buffers out of
/// [`kernel::heap`] through the OSI table, so that must be initialised first
/// — and [`crate::nvs`] too, since `nvs_enable` is 1.
///
/// Returns the blob's `esp_err_t` unchanged. Zero is `ESP_OK`; everything else
/// is Espressif's, and is more useful reported than translated.
///
/// # Safety
/// Calls into the blob, which will call back out through the OSI table on the
/// same thread. Call once.
pub unsafe fn init() -> i32 {
    let table = crate::adapter::table();
    let config = WifiInitConfig::defaults(&table);
    unsafe { esp_wifi_init_internal(&config) }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_magic_is_the_headers() {
        // Quoted rather than remembered: WIFI_INIT_CONFIG_MAGIC in esp_wifi.h.
        assert_eq!(INIT_CONFIG_MAGIC, 0x1F2F_3F4F);
    }

    #[test]
    fn the_magic_is_the_last_field() {
        // The header says "it should be the last field", and that is what
        // makes a size mismatch detectable at all: a struct laid out
        // differently puts something else where the blob reads the magic.
        // Against this build's own size, not the ESP32 constant: the property
        // is "nothing follows it", which is true at any pointer width, and
        // comparing to 200 here would only be testing the host's word size.
        assert_eq!(
            core::mem::offset_of!(WifiInitConfig, magic),
            core::mem::size_of::<WifiInitConfig>() - core::mem::size_of::<i32>()
        );
    }

    #[test]
    fn the_crypto_table_describes_its_own_size() {
        // The blob is entitled to check this, and a table claiming a size it
        // has not got is worse than one that is simply empty.
        let f = WpaCryptoFuncs::empty();
        assert_eq!(f.size as usize, core::mem::size_of::<WpaCryptoFuncs>());
    }

    #[test]
    fn no_capability_is_claimed_that_nothing_implements() {
        // Every feature_caps bit turns on a path needing something this crate
        // does not provide. WPA3 in particular would reach straight into the
        // null crypto table.
        let c = WifiInitConfig::defaults(core::ptr::null());
        assert_eq!(c.feature_caps, 0);
    }

    #[test]
    fn the_crypto_table_is_empty_and_says_so() {
        // Not a stub that pretends: WPA2 needs every one of these, and a
        // half-filled table would fail somewhere in the handshake instead of
        // at the first call.
        let f = WpaCryptoFuncs::empty();
        assert!(f.aes_wrap.is_none());
        assert!(f.pbkdf2_sha1.is_none());
        assert!(f.ccmp_encrypt.is_none());
        assert!(f.aes_gmac.is_none());
    }
}
