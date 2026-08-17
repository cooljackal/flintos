// SPDX-License-Identifier: Apache-2.0

//! The radio glue for the first-party WPA2 supplicant.
//!
//! [`crate::wifi`] registers a `wpa_funcs` table with the blob; this fills it.
//! The blob drives the six station callbacks, and this routes them through
//! [`wpa`]'s pure 4-way handshake:
//!
//! - `sta_connect` derives the PMK from the passphrase and SSID (via
//!   [`crypto::wpa_psk`]) and creates a [`wpa::Supplicant`].
//! - `sta_rx_eapol` feeds each EAPOL-Key frame to the supplicant and acts on
//!   what it returns — sending a reply through `esp_wifi_internal_tx`, and, on
//!   completion, remembering the keys to install.
//! - the keys install **after** message 4 has left, from the blob's EAPOL
//!   transmit-done callback, because installing the pairwise key any earlier
//!   would encrypt message 4 and the AP could not read it.
//!
//! # What is not host-tested
//!
//! Everything here is `unsafe` FFI into the blob and register reads: it cannot
//! be exercised off-target, and full validation is a real WPA2 AP. The logic it
//! drives — the whole handshake — is host-tested in [`wpa`]; this file is the
//! wiring, kept as thin as the blob's shape allows.

use core::ffi::c_void;

use kernel::smp::Spinlock;
use wpa::{rsn, Action, Supplicant};

use crate::wifi::WpaCallbacks;

// ── Blob FFI, signatures from esp_wifi_driver.h at v4.4 ───────────────────────

extern "C" {
    /// `int esp_wifi_internal_tx(wifi_interface_t, void*, uint16_t)`. Sends a
    /// raw L2 frame — the caller supplies the ethernet and 802.1X headers.
    fn esp_wifi_internal_tx(ifx: u32, buffer: *const c_void, len: u16) -> i32;
    /// `int esp_wifi_set_sta_key_internal(alg, addr, key_idx, set_tx, seq,
    /// seq_len, key, key_len, key_flag)`. Installs a key into the MAC.
    #[allow(clippy::too_many_arguments)]
    fn esp_wifi_set_sta_key_internal(
        alg: i32,
        addr: *const u8,
        key_idx: i32,
        set_tx: i32,
        seq: *const u8,
        seq_len: usize,
        key: *const u8,
        key_len: usize,
        key_flag: u32,
    ) -> i32;
    /// `int esp_wifi_get_macaddr_internal(uint8_t if_index, uint8_t*)`.
    fn esp_wifi_get_macaddr_internal(ifx: u8, mac: *mut u8) -> i32;
    /// `bool esp_wifi_auth_done_internal(void)`. Tells the blob the handshake
    /// completed and the connection is authorised.
    fn esp_wifi_auth_done_internal() -> bool;
    /// `int esp_wifi_register_tx_cb_internal(wifi_tx_cb_t, uint8_t id)`.
    fn esp_wifi_register_tx_cb_internal(cb: unsafe extern "C" fn(*mut c_void), id: u8) -> i32;
    /// `int esp_wifi_set_appie_internal(uint8_t type, uint8_t *ie, uint16_t len,
    /// uint8_t flag)`. Installs an application information element into the
    /// frames the blob sends — here the RSN element for the association request.
    fn esp_wifi_set_appie_internal(ty: u8, ie: *mut u8, len: u16, flag: u8) -> i32;
    /// `uint8_t esp_wifi_sta_get_prof_authmode_internal(void)`. The auth mode
    /// the blob settled on for this connection — an *internal* enum value.
    fn esp_wifi_sta_get_prof_authmode_internal() -> u8;
    /// `struct wifi_appie *esp_wifi_get_appie_internal(uint8_t type)`. Reads an
    /// installed application IE back; the struct is `{ u16 len; u8 data[] }`.
    fn esp_wifi_get_appie_internal(ty: u8) -> *const u8;
}

/// `WIFI_APPIE_RSN` — the RSN element carried in the (re)association request.
const WIFI_APPIE_RSN: u8 = 4;

/// `WIFI_IF_STA`.
const IF_STA: u32 = 0;
/// `enum wpa_alg::WIFI_WPA_ALG_CCMP`.
const ALG_CCMP: i32 = 3;
/// `enum key_flag` bits.
const KEY_FLAG_RX: u32 = 1 << 2;
const KEY_FLAG_TX: u32 = 1 << 3;
const KEY_FLAG_GROUP: u32 = 1 << 4;
const KEY_FLAG_PAIRWISE: u32 = 1 << 5;
/// The transmit-done callback id for EAPOL frames (`WIFI_TXCB_EAPOL_ID`).
const TXCB_EAPOL_ID: u8 = 3;
/// The EAPOL ethertype, big-endian.
const ETHERTYPE_EAPOL: [u8; 2] = [0x88, 0x8e];

// ── State ────────────────────────────────────────────────────────────────────

/// The live handshake, plus everything the callbacks need across calls.
///
/// One radio, one station, one handshake at a time — so a single locked slot,
/// not a per-connection allocation. The lock covers the blob's callback task
/// and the application task that seeds the credentials.
struct SupState {
    /// The running handshake, once `sta_connect` has built it.
    sup: Option<Supplicant>,
    /// The AP's address, for the ethernet header and the pairwise-key install.
    bssid: [u8; 6],
    /// This station's address.
    own_mac: [u8; 6],
    /// Keys derived on message 3, waiting for message 4 to leave before they
    /// are installed. `None` until then.
    pending: Option<PendingKeys>,
    /// The pre-shared key, derived from the passphrase and SSID by the
    /// application in [`stage_credentials`] — on its own generous stack, once,
    /// rather than in `sta_connect` on the blob's tight Wi-Fi task (which the
    /// blob calls twice per connect). `pmk_ready` gates its use; the passphrase
    /// itself is never kept here.
    pmk: [u8; 32],
    pmk_ready: bool,
}

/// The two keys to install once message 4 is out.
#[derive(Clone, Copy)]
struct PendingKeys {
    tk: [u8; 16],
    gtk: [u8; 32],
    gtk_len: usize,
    gtk_id: u8,
}

static STATE: Spinlock<SupState> = Spinlock::new(SupState {
    sup: None,
    bssid: [0; 6],
    own_mac: [0; 6],
    pending: None,
    pmk: [0; 32],
    pmk_ready: false,
});

/// Stage the network the application is about to connect to. Derives the PMK
/// here — on the caller's (application) stack, where PBKDF2's 4096 iterations
/// have room — and keeps only the 32-byte result, so `sta_connect` need not run
/// it on the blob's Wi-Fi task. Called by [`crate::station`]'s `connect`.
pub(crate) fn stage_credentials(ssid: &[u8], passphrase: &[u8]) {
    let pmk = crypto::wpa_psk(passphrase, ssid);
    STATE.with(|st| {
        st.pmk = pmk;
        st.pmk_ready = true;
    });
}

// ── Randomness for the SNonce ────────────────────────────────────────────────

/// The ESP32 hardware RNG data register. Good entropy while the radio is up,
/// which it is throughout a handshake — see `drivers/physical/esp32/rng`, whose
/// layer this file may not depend on, so the register is read directly here as
/// the radio reads its own.
const RNG_DATA_REG: usize = 0x3FF7_5144;

struct HwRng;
impl wpa::Rng for HwRng {
    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(4) {
            let word = unsafe { (RNG_DATA_REG as *const u32).read_volatile() };
            let bytes = word.to_ne_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}

// ── The six station callbacks ────────────────────────────────────────────────

unsafe extern "C" fn sta_init() -> bool {
    // Register the EAPOL transmit-done callback once, here, as esp-idf's
    // supplicant does in wpa_attach.
    unsafe { esp_wifi_register_tx_cb_internal(eapol_tx_done, TXCB_EAPOL_ID) };
    true
}

unsafe extern "C" fn sta_deinit() -> bool {
    STATE.with(|st| {
        st.sup = None;
        st.pending = None;
    });
    true
}

/// `wpa_sta_connect`: build the handshake for the AP at `bssid`.
unsafe extern "C" fn sta_connect(bssid: *mut u8) -> i32 {
    if bssid.is_null() {
        return -1;
    }
    let mut aa = [0u8; 6];
    aa.copy_from_slice(unsafe { core::slice::from_raw_parts(bssid, 6) });

    let mut own = [0u8; 6];
    if unsafe { esp_wifi_get_macaddr_internal(0, own.as_mut_ptr()) } != 0 {
        return -1;
    }

    let ready = STATE.with(|st| {
        if !st.pmk_ready {
            return false;
        }
        st.bssid = aa;
        st.own_mac = own;
        st.pending = None;
        st.sup = Some(Supplicant::new(st.pmk, aa, own));
        true
    });
    if !ready {
        api::log_error!("[wpa] sta_connect with no PMK staged");
        return -1;
    }

    // Install the station's RSN IE for the association request — the same
    // element the handshake presents in message 2. esp-idf does this in
    // `wpa_config_bss` → `wpa_config_assoc_ie`; the first-party `sta_connect`
    // replaces that C path, so it must install the IE itself or the association
    // request goes out with no security element and a secured AP rejects it.
    //
    // The last argument is the *flag*, and it is not "enable": flag=1 means the
    // pointer already addresses a persistent `wifi_appie { u16 len; u8 data[] }`
    // whose IE begins at byte 2 — esp-idf's `set_assoc_ie` reserves those two
    // bytes (`assoc_ie_buf[LEN+2]`, `assoc_wpa_ie = buf + 2`). Passing our raw
    // 22-byte element with flag=1 made the blob write the length over our
    // `0x30,0x14` element header and transmit a malformed IE, which the AP
    // rejected. flag=0 is the copy path: the blob allocates len+2, copies our
    // bytes after its own length field, and owns the result — so a stack buffer
    // is fine (esp-idf uses flag=0 with a buffer it frees right after). Done
    // outside the lock: it calls into the blob, which may arm timers of its own.
    let mut ie = wpa::keydata::RSN_IE_WPA2_PSK_CCMP;
    let set_rc =
        unsafe { esp_wifi_set_appie_internal(WIFI_APPIE_RSN, ie.as_mut_ptr(), ie.len() as u16, 0) };
    api::log_info!("[wpa] sta_connect: assoc IE install rc={}", set_rc);

    // Diagnostic: read the IE back out of slot 4 and confirm the blob retained
    // exactly the 22 bytes we installed, before it builds the association. The
    // returned `struct wifi_appie` is { u16 len; u8 data[] }.
    let stored = unsafe { esp_wifi_get_appie_internal(WIFI_APPIE_RSN) };
    if stored.is_null() {
        api::log_warn!("[wpa] assoc IE slot 4 empty after install");
    } else {
        let len = unsafe { stored.cast::<u16>().read_unaligned() } as usize;
        let data = unsafe { stored.cast::<u8>().add(2) };
        let got = unsafe { core::slice::from_raw_parts(data, len.min(ie.len())) };
        let matches = len == ie.len() && got == &ie[..];
        api::log_info!("[wpa] assoc IE stored len={} matches={}", len, matches);
    }

    // Diagnostic: the auth mode the blob settled on. This is the *internal*
    // enum (esp_wifi_driver.h), not the public wifi_auth_mode_t: WPA2_AUTH_PSK
    // is 5, WPA3_AUTH_PSK (SAE) is 9. It should read 5 now the parser hides SAE.
    let authmode = unsafe { esp_wifi_sta_get_prof_authmode_internal() };
    api::log_info!("[wpa] authmode={}", authmode);
    0
}

unsafe extern "C" fn sta_disconnected(_reason: u8) {
    STATE.with(|st| {
        st.sup = None;
        st.pending = None;
    });
}

/// `wpa_sta_rx_eapol`: drive the handshake with one received frame.
///
/// `buf` starts at the 802.1X header; `len` covers it and the key body. Returns
/// 1 when the frame was a handled EAPOL-Key, matching the blob's expectation.
unsafe extern "C" fn sta_rx_eapol(_src: *mut u8, buf: *mut u8, len: u32) -> i32 {
    if buf.is_null() {
        return -1;
    }
    let frame = unsafe { core::slice::from_raw_parts(buf, len as usize) };
    api::log_info!("[wpa] rx eapol: {} bytes", len);

    // A place to copy the outgoing frame to, so the supplicant's borrow of its
    // own buffer ends before we transmit. Handshake frames are ~120 bytes.
    let mut tx = [0u8; 300];
    let mut tx_len = 0usize;
    let mut dest = [0u8; 6];
    let mut label = "ignored";

    STATE.with(|st| {
        let Some(sup) = st.sup.as_mut() else {
            label = "no supplicant";
            return;
        };
        dest = st.bssid;
        let mut rng = HwRng;
        match sup.on_eapol(frame, &mut rng) {
            Action::None => {}
            Action::Send(reply) => {
                label = "sent reply";
                tx_len = frame_into(&mut tx, &dest, &st.own_mac, reply);
            }
            Action::Complete { reply, tk, gtk } => {
                label = "complete; msg4 sent";
                tx_len = frame_into(&mut tx, &dest, &st.own_mac, reply);
                // Hold the keys until message 4 has actually been sent.
                st.pending = Some(PendingKeys {
                    tk,
                    gtk: gtk.key,
                    gtk_len: gtk.len,
                    gtk_id: gtk.key_id,
                });
            }
        }
    });

    if tx_len > 0 {
        unsafe { esp_wifi_internal_tx(IF_STA, tx.as_ptr() as *const c_void, tx_len as u16) };
    }
    api::log_info!("[wpa] eapol handled: {}", label);
    1
}

unsafe extern "C" fn sta_in_4way_handshake() -> bool {
    STATE.with(|st| {
        matches!(
            st.sup.as_ref().map(|s| s.state()),
            Some(wpa::State::AwaitingThree)
        )
    })
}

// ── EAPOL transmit-done: install keys after message 4 ────────────────────────

/// The blob's EAPOL transmit-done callback. It fires after every EAPOL frame
/// this station sends; only after message 4 is there anything to do, and that
/// is exactly when `pending` is set.
unsafe extern "C" fn eapol_tx_done(_arg: *mut c_void) {
    let keys = STATE.with(|st| st.pending.take());
    let Some(keys) = keys else {
        return; // message 2's completion — nothing pending
    };
    api::log_info!("[wpa] eapol tx done: installing PTK + GTK, auth done");
    let (bssid, _own) = STATE.with(|st| (st.bssid, st.own_mac));

    // Pairwise key (PTK's TK): AES-CCMP, keyed to the AP, index 0, TX.
    unsafe {
        esp_wifi_set_sta_key_internal(
            ALG_CCMP,
            bssid.as_ptr(),
            0,
            1,
            core::ptr::null(),
            0,
            keys.tk.as_ptr(),
            keys.tk.len(),
            KEY_FLAG_PAIRWISE | KEY_FLAG_TX,
        );
    }
    // Group key (GTK): AES-CCMP, no peer address, its own index, RX. The
    // receive sequence counter starts at zero here; the AP's RSC would refine
    // replay detection and is a later refinement.
    let seq = [0u8; 6];
    unsafe {
        esp_wifi_set_sta_key_internal(
            ALG_CCMP,
            core::ptr::null(),
            keys.gtk_id as i32,
            0,
            seq.as_ptr(),
            seq.len(),
            keys.gtk.as_ptr(),
            keys.gtk_len,
            KEY_FLAG_GROUP | KEY_FLAG_RX,
        );
    }

    // Both keys in: tell the blob the connection is authorised.
    unsafe { esp_wifi_auth_done_internal() };
}

/// Build an EAPOL L2 frame into `out`: a 14-byte ethernet header (dest, src,
/// EAPOL ethertype) followed by the 802.1X payload. Returns the total length.
fn frame_into(out: &mut [u8], dest: &[u8; 6], src: &[u8; 6], payload: &[u8]) -> usize {
    let total = 14 + payload.len();
    if out.len() < total {
        return 0;
    }
    out[0..6].copy_from_slice(dest);
    out[6..12].copy_from_slice(src);
    out[12..14].copy_from_slice(&ETHERTYPE_EAPOL);
    out[14..total].copy_from_slice(payload);
    total
}

// ── RSN/WPA information-element parsing ───────────────────────────────────────

/// `wifi_wpa_ie_t` from esp_wifi_driver.h at v4.4 — where the blob wants a
/// beacon's security written. The layout is asserted, not trusted; the blob
/// reads these exact fields at these exact offsets.
///
/// The value encoding matches `wpa_parse_wpa_ie_wrapper`: `proto` and `key_mgmt`
/// are the supplicant's internal bitmasks, the cipher fields are the public
/// `WIFI_CIPHER_TYPE_*` numbers — which is what [`rsn::parse`] already produces.
#[repr(C)]
struct WifiWpaIe {
    proto: i32,
    pairwise_cipher: i32,
    group_cipher: i32,
    key_mgmt: i32,
    capabilities: i32,
    num_pmkid: usize,
    pmkid: *const u8,
    mgmt_group_cipher: i32,
}

// The v4.4 layout on the 32-bit target, asserted rather than trusted. The
// struct carries a `usize` and a pointer, so it is only 32 bytes where those
// are four each — the target ABI. On a 64-bit host (where this crate still
// compiles for its unit tests) it is 40, and the assert would be checking a
// layout the blob never sees, so it is scoped to the width that ships.
#[cfg(target_pointer_width = "32")]
const _: () = {
    use core::mem::offset_of;
    assert!(core::mem::size_of::<WifiWpaIe>() == 32);
    assert!(offset_of!(WifiWpaIe, proto) == 0);
    assert!(offset_of!(WifiWpaIe, pairwise_cipher) == 4);
    assert!(offset_of!(WifiWpaIe, group_cipher) == 8);
    assert!(offset_of!(WifiWpaIe, key_mgmt) == 12);
    assert!(offset_of!(WifiWpaIe, capabilities) == 16);
    assert!(offset_of!(WifiWpaIe, num_pmkid) == 20);
    assert!(offset_of!(WifiWpaIe, pmkid) == 24);
    assert!(offset_of!(WifiWpaIe, mgmt_group_cipher) == 28);
};

/// `wpa_parse_wpa_ie`: classify a network's security from its RSN/WPA element.
///
/// `wpa_ie` starts at the element id and runs `wpa_ie_len` bytes; `data` is a
/// `wifi_wpa_ie_t` to fill. Returns 0 when the element parses, -1 when it is
/// malformed — matching the wrapper this replaces. The output struct is written
/// only after [`rsn::parse`] has validated the whole element, so a malformed
/// beacon cannot leave partially trusted security behind.
unsafe extern "C" fn parse_wpa_ie(wpa_ie: *const u8, wpa_ie_len: usize, data: *mut c_void) -> i32 {
    if wpa_ie.is_null() || data.is_null() {
        return -1;
    }
    let ie = unsafe { core::slice::from_raw_parts(wpa_ie, wpa_ie_len) };
    let Some(info) = rsn::parse(ie) else {
        return -1;
    };

    // Advertise to the blob only the AKMs this build can actually run. The
    // parser faithfully reports what the AP offers — on a WPA2/WPA3-transitional
    // network that is PSK|SAE — but we implement WPA2-PSK only. Handing the blob
    // SAE lets it select the WPA3 side of transition mode, whose authentication
    // we cannot complete; masking to PSK keeps the connection on the WPA2 path.
    // An AP with no AKM we support is unusable to us: report it unparsed so it
    // is not offered as connectable.
    const SUPPORTED_AKM: u32 = rsn::akm::PSK;
    let key_mgmt = info.key_mgmt & SUPPORTED_AKM;
    if key_mgmt == 0 {
        return -1;
    }

    // Diagnostic: the AP's key management (raw vs the masked value we report)
    // and RSN capabilities. caps bit 7 (0x0080) is MFPC (PMF capable), bit 6
    // (0x0040) is MFPR (PMF required) — the pair that decides whether a non-PMF
    // WPA2 station can associate at all.
    api::log_info!(
        "[wpa] parse_wpa_ie: km={:#06x}->{:#06x} pair={} grp={} caps={:#06x}",
        info.key_mgmt,
        key_mgmt,
        info.pairwise_cipher,
        info.group_cipher,
        info.capabilities
    );

    // Only now, with the element fully validated, populate the struct.
    let out = data.cast::<WifiWpaIe>();
    let pmkid = match info.pmkid_offset {
        Some(off) => unsafe { wpa_ie.add(off) },
        None => core::ptr::null(),
    };
    unsafe {
        (*out).proto = info.proto as i32;
        (*out).pairwise_cipher = info.pairwise_cipher as i32;
        (*out).group_cipher = info.group_cipher as i32;
        (*out).key_mgmt = key_mgmt as i32;
        (*out).capabilities = info.capabilities as i32;
        (*out).num_pmkid = info.num_pmkid;
        (*out).pmkid = pmkid;
        (*out).mgmt_group_cipher = info.mgmt_group_cipher as i32;
    }
    0
}

// ── Registration ─────────────────────────────────────────────────────────────

/// The full `wpa_funcs` table: the six station callbacks driving the handshake,
/// no-ops for the AP and WPA3 entries the personal-station path never reaches.
static mut CALLBACKS: WpaCallbacks = WpaCallbacks {
    sta_init: Some(sta_init),
    sta_deinit: Some(sta_deinit),
    sta_connect: Some(sta_connect),
    sta_disconnected: Some(sta_disconnected),
    sta_rx_eapol: Some(sta_rx_eapol),
    sta_in_4way_handshake: Some(sta_in_4way_handshake),
    ap_init: None,
    ap_deinit: None,
    ap_join: None,
    ap_remove: None,
    ap_get_wpa_ie: None,
    ap_rx_eapol: None,
    ap_get_peer_spp_msg: None,
    config_parse_string: None,
    parse_wpa_ie: Some(parse_wpa_ie),
    config_bss: None,
    michael_mic_failure: None,
    wpa3_build_sae_msg: None,
    wpa3_parse_sae_msg: None,
    sta_rx_mgmt: None,
    config_done: Some(config_done),
    // NULL, as esp-idf's non-roaming, non-MBO build leaves it (esp_wpa_main.c
    // and esp_common.c both). A stub returning false here reads to the blob as
    // "this candidate does not match my profile" and rejects every scanned AP —
    // a visible, correctly classified network then fails connect with
    // NoApFound before the handshake is ever reached.
    sta_profile_match: None,
};

unsafe extern "C" fn config_done() {}

/// Register the supplicant's callback table with the blob. Called once, from
/// [`crate::wifi::init`], in place of the old scan-only stubs.
///
/// # Safety
/// Calls into the blob; `esp_wifi_init_internal` must have succeeded.
pub(crate) unsafe fn register() -> i32 {
    unsafe { crate::wifi::esp_wifi_register_wpa_cb_internal(core::ptr::addr_of_mut!(CALLBACKS)) }
}
