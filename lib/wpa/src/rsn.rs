// SPDX-License-Identifier: Apache-2.0

//! Parsing a network's security from its RSN / WPA information element.
//!
//! The blob hands the supplicant a beacon's RSN element (or the legacy WPA
//! vendor element) and asks what security it describes — which cipher, which
//! key management. Without an answer every network reads as open and no
//! secured AP can be joined. This is that answer: a bounds-checked,
//! allocation-free parser that turns the element's bytes into the fields the
//! driver expects.
//!
//! # The two elements
//!
//! - **RSN** (element id 48, IEEE 802.11i): WPA2 and WPA3. Suites carry the
//!   `00-0F-AC` OUI.
//! - **WPA** (element id 221, vendor-specific with OUI `00-50-F2` type 1): the
//!   original WPA. Suites carry the `00-50-F2` OUI.
//!
//! # The value encoding is the reference's, not the standard's
//!
//! The cipher fields come out as esp-idf's **public** `WIFI_CIPHER_TYPE_*`
//! numbers, and `key_mgmt` and `proto` as its **internal** bitmasks — exactly
//! what `wpa_parse_wpa_ie_wrapper` produces, because the blob reads the result
//! and was built against that wrapper. The mappings below are transcribed from
//! `wpa.c`'s `cipher_type_map_supp_to_public` and `defs.h`.
//!
//! # Validate, then emit
//!
//! Every length is checked before it is read, and the [`SecurityInfo`] is built
//! only once the whole element has validated — a truncated or malformed element
//! returns `None` and never leaves half-trusted security state behind.

/// esp-idf public `WIFI_CIPHER_TYPE_*` — what the cipher fields hold.
pub mod cipher {
    pub const NONE: u32 = 0;
    pub const WEP40: u32 = 1;
    pub const WEP104: u32 = 2;
    pub const TKIP: u32 = 3;
    pub const CCMP: u32 = 4;
    pub const TKIP_CCMP: u32 = 5;
    pub const AES_CMAC128: u32 = 6;
    pub const UNKNOWN: u32 = 12;
}

/// Internal `WPA_KEY_MGMT_*` bitmask — what `key_mgmt` holds.
pub mod akm {
    pub const IEEE8021X: u32 = 1 << 0;
    pub const PSK: u32 = 1 << 1;
    pub const PSK_SHA256: u32 = 1 << 8;
    pub const SAE: u32 = 1 << 10;
}

/// Internal `WPA_PROTO_*`.
pub mod proto {
    pub const WPA: u32 = 1 << 0;
    pub const RSN: u32 = 1 << 1;
}

/// Internal `WPA_CIPHER_*` bits, accumulated before mapping to a public value.
mod wpa_cipher {
    pub const NONE: u32 = 1 << 0;
    pub const TKIP: u32 = 1 << 1;
    pub const CCMP: u32 = 1 << 3;
    pub const AES_128_CMAC: u32 = 1 << 5;
}

/// Element ids.
const EID_RSN: u8 = 48;
const EID_VENDOR: u8 = 221;
/// OUIs.
const OUI_RSN: [u8; 3] = [0x00, 0x0f, 0xac];
const OUI_WPA: [u8; 3] = [0x00, 0x50, 0xf2];
/// The WPA vendor element's type byte after the OUI.
const WPA_OUI_TYPE: u8 = 1;
/// A cipher/AKM suite selector is a 4-byte OUI + type.
const SELECTOR_LEN: usize = 4;
/// A PMKID is 16 bytes.
const PMKID_LEN: usize = 16;

/// The security an RSN/WPA element describes, in the driver's value encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecurityInfo {
    /// `WPA_PROTO_WPA` or `WPA_PROTO_RSN`.
    pub proto: u32,
    /// Pairwise cipher, as a public `WIFI_CIPHER_TYPE_*`.
    pub pairwise_cipher: u32,
    /// Group cipher, public.
    pub group_cipher: u32,
    /// Key-management, as an internal `WPA_KEY_MGMT_*` bitmask (may hold
    /// several, e.g. PSK and SAE for a transitional network).
    pub key_mgmt: u32,
    /// The raw RSN capabilities word, 0 if absent (WPA has none).
    pub capabilities: u32,
    /// How many PMKIDs the element carried.
    pub num_pmkid: usize,
    /// Byte offset of the PMKID list within the parsed element, if any. The
    /// caller turns this into a pointer; the parser holds no pointers.
    pub pmkid_offset: Option<usize>,
    /// Group management cipher (PMF), public; `NONE` if absent.
    pub mgmt_group_cipher: u32,
}

/// Map one suite selector to an internal cipher bit. `None` for an OUI or type
/// this does not recognise, which the caller treats as unsupported.
fn cipher_bit(sel: &[u8]) -> Option<u32> {
    if sel.len() != SELECTOR_LEN {
        return None;
    }
    let oui = &sel[..3];
    let ty = sel[3];
    if oui != OUI_RSN && oui != OUI_WPA {
        return None; // an unknown OUI is not ours to classify
    }
    Some(match ty {
        0 => wpa_cipher::NONE,
        2 => wpa_cipher::TKIP,
        4 => wpa_cipher::CCMP,
        6 => wpa_cipher::AES_128_CMAC,
        _ => return None,
    })
}

/// Map an accumulated internal cipher bitmask to a public `WIFI_CIPHER_TYPE_*`,
/// exactly as `cipher_type_map_supp_to_public` does.
fn cipher_to_public(bits: u32) -> u32 {
    match bits {
        wpa_cipher::NONE => cipher::NONE,
        wpa_cipher::TKIP => cipher::TKIP,
        wpa_cipher::CCMP => cipher::CCMP,
        x if x == wpa_cipher::CCMP | wpa_cipher::TKIP => cipher::TKIP_CCMP,
        wpa_cipher::AES_128_CMAC => cipher::AES_CMAC128,
        _ => cipher::UNKNOWN,
    }
}

/// Map one AKM suite selector to an internal key-management bit.
fn akm_bit(sel: &[u8]) -> Option<u32> {
    if sel.len() != SELECTOR_LEN {
        return None;
    }
    let oui = &sel[..3];
    let ty = sel[3];
    if oui != OUI_RSN && oui != OUI_WPA {
        return None;
    }
    Some(match ty {
        1 => akm::IEEE8021X,
        2 => akm::PSK,
        6 => akm::PSK_SHA256,
        8 => akm::SAE,
        _ => return None,
    })
}

/// A little-endian u16 at `off`, if it fits.
fn le16(buf: &[u8], off: usize) -> Option<u16> {
    if off + 2 > buf.len() {
        return None;
    }
    Some(u16::from_le_bytes([buf[off], buf[off + 1]]))
}

/// Parse an RSN (id 48) or WPA (id 221) information element into a
/// [`SecurityInfo`]. `ie` starts at the element id. `None` if it is neither, or
/// is malformed or truncated.
pub fn parse(ie: &[u8]) -> Option<SecurityInfo> {
    if ie.len() < 2 {
        return None;
    }
    let eid = ie[0];
    let len = ie[1] as usize;
    if ie.len() < 2 + len {
        return None; // the declared body runs past the buffer
    }

    match eid {
        EID_RSN => parse_body(ie, 2, proto::RSN),
        EID_VENDOR => {
            // WPA vendor element: OUI(3) + type(1) must be 00-50-F2 / 1.
            if len < 4 || ie[2..5] != OUI_WPA || ie[5] != WPA_OUI_TYPE {
                return None; // some other vendor element — not security
            }
            parse_body(ie, 6, proto::WPA)
        }
        _ => None,
    }
}

/// Parse from the version field onward. `start` points at the 2-byte version.
fn parse_body(ie: &[u8], start: usize, proto: u32) -> Option<SecurityInfo> {
    let end = 2 + ie[1] as usize; // one past the element's declared body

    // version (2, LE) = 1. Present on both element types.
    let _version = le16(ie, start)?;
    let mut pos = start + 2;

    // Group cipher suite (one selector).
    if pos + SELECTOR_LEN > end {
        return None;
    }
    let group_bits = cipher_bit(&ie[pos..pos + SELECTOR_LEN]).unwrap_or(wpa_cipher::NONE);
    pos += SELECTOR_LEN;

    // Pairwise cipher suite list.
    let pw_count = le16(ie, pos)? as usize;
    pos += 2;
    let mut pw_bits = 0u32;
    for _ in 0..pw_count {
        if pos + SELECTOR_LEN > end {
            return None;
        }
        if let Some(b) = cipher_bit(&ie[pos..pos + SELECTOR_LEN]) {
            pw_bits |= b;
        }
        pos += SELECTOR_LEN;
    }

    // AKM (key management) suite list.
    let akm_count = le16(ie, pos)? as usize;
    pos += 2;
    let mut km = 0u32;
    for _ in 0..akm_count {
        if pos + SELECTOR_LEN > end {
            return None;
        }
        if let Some(b) = akm_bit(&ie[pos..pos + SELECTOR_LEN]) {
            km |= b;
        }
        pos += SELECTOR_LEN;
    }

    // Everything past here is optional (RSN only; WPA stops here).
    let mut capabilities = 0u32;
    let mut num_pmkid = 0usize;
    let mut pmkid_offset = None;
    let mut mgmt_group_cipher = cipher::NONE;

    if pos + 2 <= end {
        capabilities = le16(ie, pos)? as u32;
        pos += 2;
    }
    if pos + 2 <= end {
        num_pmkid = le16(ie, pos)? as usize;
        pos += 2;
        if num_pmkid > 0 {
            let bytes = num_pmkid.checked_mul(PMKID_LEN)?;
            if pos + bytes > end {
                return None;
            }
            pmkid_offset = Some(pos);
            pos += bytes;
        }
    }
    if pos + SELECTOR_LEN <= end {
        let bits = cipher_bit(&ie[pos..pos + SELECTOR_LEN]).unwrap_or(wpa_cipher::NONE);
        mgmt_group_cipher = cipher_to_public(bits);
    }

    // Validated in full — now, and only now, produce the result.
    Some(SecurityInfo {
        proto,
        pairwise_cipher: cipher_to_public(pw_bits),
        group_cipher: cipher_to_public(group_bits),
        key_mgmt: km,
        capabilities,
        num_pmkid,
        pmkid_offset,
        mgmt_group_cipher,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Selector helpers.
    fn rsn_suite(ty: u8) -> [u8; 4] {
        [0x00, 0x0f, 0xac, ty]
    }
    fn wpa_suite(ty: u8) -> [u8; 4] {
        [0x00, 0x50, 0xf2, ty]
    }

    // Build an RSN element: id 48, len, version, group, pairwise list, akm list,
    // then optional trailing bytes appended by the caller.
    fn rsn_ie(group: u8, pairwise: &[u8], akm: &[u8], tail: &[u8]) -> [u8; 64] {
        let mut b = [0u8; 64];
        let mut n = 2; // id + len filled last
        b[n..n + 2].copy_from_slice(&1u16.to_le_bytes()); // version
        n += 2;
        b[n..n + 4].copy_from_slice(&rsn_suite(group));
        n += 4;
        b[n..n + 2].copy_from_slice(&(pairwise.len() as u16).to_le_bytes());
        n += 2;
        for &t in pairwise {
            b[n..n + 4].copy_from_slice(&rsn_suite(t));
            n += 4;
        }
        b[n..n + 2].copy_from_slice(&(akm.len() as u16).to_le_bytes());
        n += 2;
        for &t in akm {
            b[n..n + 4].copy_from_slice(&rsn_suite(t));
            n += 4;
        }
        b[n..n + tail.len()].copy_from_slice(tail);
        n += tail.len();
        b[0] = EID_RSN;
        b[1] = (n - 2) as u8;
        b
    }

    #[test]
    fn wpa2_psk_ccmp() {
        // group CCMP(4), pairwise [CCMP], akm [PSK(2)].
        let ie = rsn_ie(4, &[4], &[2], &[]);
        let s = parse(&ie).unwrap();
        assert_eq!(s.proto, proto::RSN);
        assert_eq!(s.pairwise_cipher, cipher::CCMP);
        assert_eq!(s.group_cipher, cipher::CCMP);
        assert_eq!(s.key_mgmt, akm::PSK);
    }

    #[test]
    fn wpa2_wpa3_transition_keeps_both_akms() {
        // pairwise CCMP, akm [PSK(2), SAE(8)] — a transitional network.
        let ie = rsn_ie(4, &[4], &[2, 8], &[]);
        let s = parse(&ie).unwrap();
        assert_eq!(s.key_mgmt, akm::PSK | akm::SAE);
        assert_eq!(s.pairwise_cipher, cipher::CCMP);
    }

    #[test]
    fn mixed_tkip_and_ccmp_pairwise_maps_to_tkip_ccmp() {
        let ie = rsn_ie(2, &[2, 4], &[2], &[]);
        let s = parse(&ie).unwrap();
        assert_eq!(s.pairwise_cipher, cipher::TKIP_CCMP);
        assert_eq!(s.group_cipher, cipher::TKIP);
    }

    #[test]
    fn legacy_wpa_vendor_element() {
        // WPA element (221): OUI 00-50-F2 type 1, version, group TKIP, pairwise
        // [TKIP], akm [PSK].
        let mut b = [0u8; 64];
        let mut n = 2;
        b[n..n + 3].copy_from_slice(&OUI_WPA);
        n += 3;
        b[n] = WPA_OUI_TYPE;
        n += 1;
        b[n..n + 2].copy_from_slice(&1u16.to_le_bytes());
        n += 2;
        b[n..n + 4].copy_from_slice(&wpa_suite(2)); // group TKIP
        n += 4;
        b[n..n + 2].copy_from_slice(&1u16.to_le_bytes());
        n += 2;
        b[n..n + 4].copy_from_slice(&wpa_suite(2)); // pairwise TKIP
        n += 4;
        b[n..n + 2].copy_from_slice(&1u16.to_le_bytes());
        n += 2;
        b[n..n + 4].copy_from_slice(&wpa_suite(2)); // akm PSK
        n += 4;
        b[0] = EID_VENDOR;
        b[1] = (n - 2) as u8;
        let s = parse(&b).unwrap();
        assert_eq!(s.proto, proto::WPA);
        assert_eq!(s.pairwise_cipher, cipher::TKIP);
        assert_eq!(s.key_mgmt, akm::PSK);
    }

    #[test]
    fn capabilities_and_pmkid_and_group_mgmt() {
        // Trailing: capabilities(2) + pmkid count(2)=1 + one PMKID(16) + group
        // mgmt cipher CMAC(6).
        let mut tail = [0u8; 2 + 2 + 16 + 4];
        tail[0..2].copy_from_slice(&0x00ccu16.to_le_bytes());
        tail[2..4].copy_from_slice(&1u16.to_le_bytes());
        for (i, b) in tail[4..20].iter_mut().enumerate() {
            *b = i as u8;
        }
        tail[20..24].copy_from_slice(&rsn_suite(6)); // BIP-CMAC-128
        let ie = rsn_ie(4, &[4], &[2], &tail);
        let s = parse(&ie).unwrap();
        assert_eq!(s.capabilities, 0x00cc);
        assert_eq!(s.num_pmkid, 1);
        assert!(s.pmkid_offset.is_some());
        assert_eq!(s.mgmt_group_cipher, cipher::AES_CMAC128);
    }

    #[test]
    fn malformed_is_rejected_not_partially_parsed() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[48]).is_none()); // id, no len
        assert!(parse(&[48, 40, 1, 0]).is_none()); // len claims 40, buffer has 2
        // A pairwise count that overruns the element.
        let mut ie = rsn_ie(4, &[4], &[2], &[]);
        let pw_count_off = 2 + 2 + 4; // id,len,version,group
        ie[pw_count_off] = 0xff; // claim 255 pairwise suites
        assert!(parse(&ie).is_none());
        // A non-security vendor element (wrong OUI) is not ours.
        assert!(parse(&[221, 4, 0x00, 0x11, 0x22, 0x01]).is_none());
    }

    #[test]
    fn an_open_network_has_no_element() {
        // Open networks carry no RSN/WPA element; the caller passes an empty or
        // non-matching slice and gets None, which it reports as "not secured".
        assert!(parse(&[]).is_none());
        assert!(parse(&[0x00, 0x00]).is_none()); // SSID element, say
    }
}
