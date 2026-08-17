// SPDX-License-Identifier: Apache-2.0

//! The key-data field: the station's RSN IE, and the AP's GTK.
//!
//! Two small, fixed binary shapes live here. On the way out, message 2 carries
//! the station's **RSN information element** — its declaration of which ciphers
//! and key-management it wants. On the way in, message 3's key data (once
//! decrypted) carries the **group key** wrapped in a KDE, a
//! type-length-value with the RSN OUI. Both are transcribed from the reference
//! rather than the standard's prose.

/// The RSN OUI, `00-0F-AC`, that prefixes every RSN cipher/AKM/KDE selector.
const RSN_OUI: [u8; 3] = [0x00, 0x0f, 0xac];

/// Cipher suite type 4: CCMP (AES). The only data cipher handled here.
const CIPHER_CCMP: u8 = 4;
/// AKM suite type 2: PSK. WPA2-Personal.
const AKM_PSK: u8 = 2;

/// The station's RSN IE for WPA2-PSK with CCMP, for message 2's key data.
///
/// Fixed for this configuration: version 1, group cipher CCMP, one pairwise
/// cipher (CCMP), one AKM (PSK), no RSN capabilities set. The AP compares this
/// against what it advertised; a mismatch fails the handshake, so the bytes
/// must be exactly what the reference sends for the same configuration.
pub const RSN_IE_WPA2_PSK_CCMP: [u8; 22] = [
    0x30, // element id: RSN
    0x14, // length: 20 bytes follow
    0x01, 0x00, // version 1, little-endian
    RSN_OUI[0], RSN_OUI[1], RSN_OUI[2], CIPHER_CCMP, // group cipher: CCMP
    0x01, 0x00, // pairwise cipher count: 1
    RSN_OUI[0], RSN_OUI[1], RSN_OUI[2], CIPHER_CCMP, // pairwise cipher: CCMP
    0x01, 0x00, // AKM count: 1
    RSN_OUI[0], RSN_OUI[1], RSN_OUI[2], AKM_PSK, // AKM: PSK
    0x00, 0x00, // RSN capabilities: none
];

/// A group key recovered from message 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gtk {
    /// The key material — 16 bytes for CCMP.
    pub key: [u8; 32],
    /// How many of `key` are valid.
    pub len: usize,
    /// Which group-key slot (0..3) this occupies.
    pub key_id: u8,
}

/// Find and parse the GTK KDE inside decrypted message-3 key data.
///
/// The key data is a sequence of elements. The GTK arrives as a vendor-specific
/// KDE: `0xDD, len, 00-0F-AC, 01, key_id/tx byte, reserved, GTK…`. Other
/// elements (the AP's RSN IE, padding) are skipped. `None` if no GTK KDE is
/// present or it is malformed.
pub fn find_gtk(key_data: &[u8]) -> Option<Gtk> {
    /// KKDE data type 1 within the RSN OUI is the GTK.
    const GTK_KDE_TYPE: u8 = 1;
    /// The KDE header before its payload: type, len, OUI(3), data-type.
    const KDE_HDR: usize = 6;

    let mut i = 0;
    while i + 2 <= key_data.len() {
        let id = key_data[i];
        let len = key_data[i + 1] as usize;
        let body_start = i + 2;
        let body_end = body_start + len;
        if body_end > key_data.len() {
            return None; // a length that runs off the end is malformed
        }

        // 0xDD is a vendor-specific element (the KDE carrier); 0x00 is padding.
        if id == 0xDD && len >= 4 {
            let oui = &key_data[body_start..body_start + 3];
            let data_type = key_data[body_start + 3];
            if oui == RSN_OUI && data_type == GTK_KDE_TYPE {
                // Payload: key_id (bits 0..1) | tx (bit 2), a reserved byte,
                // then the GTK.
                let payload = &key_data[body_start + 4..body_end];
                if payload.len() < 2 {
                    return None;
                }
                let key_id = payload[0] & 0x03;
                let gtk = &payload[2..];
                if gtk.is_empty() || gtk.len() > 32 {
                    return None;
                }
                let mut key = [0u8; 32];
                key[..gtk.len()].copy_from_slice(gtk);
                return Some(Gtk {
                    key,
                    len: gtk.len(),
                    key_id,
                });
            }
        }

        if len == 0 {
            // A zero-length element with a non-padding id would loop forever.
            i += 1;
        } else {
            i = body_end;
        }
        // Skip the KDE header accounting cleanly for the next element.
        let _ = KDE_HDR;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rsn_ie_has_the_documented_shape() {
        // Element id, length, and the three RSN-OUI selectors for CCMP/CCMP/PSK.
        assert_eq!(RSN_IE_WPA2_PSK_CCMP[0], 0x30);
        assert_eq!(RSN_IE_WPA2_PSK_CCMP[1], 0x14);
        assert_eq!(RSN_IE_WPA2_PSK_CCMP.len(), 22);
        // group, pairwise, akm selectors all carry the RSN OUI.
        assert_eq!(&RSN_IE_WPA2_PSK_CCMP[4..7], &RSN_OUI);
        assert_eq!(RSN_IE_WPA2_PSK_CCMP[7], CIPHER_CCMP);
        assert_eq!(RSN_IE_WPA2_PSK_CCMP[13], CIPHER_CCMP);
        assert_eq!(RSN_IE_WPA2_PSK_CCMP[19], AKM_PSK);
    }

    /// A 16-byte-GTK KDE as a fixed 24-byte array: 0xDD, len, OUI(3), type 1,
    /// [keyid|tx], reserved, then the GTK.
    fn kde16(gtk: &[u8; 16], key_id: u8, tx: bool) -> [u8; 24] {
        let mut v = [0u8; 24];
        v[0] = 0xDD;
        v[1] = (4 + 2 + 16) as u8; // OUI(3)+type(1)+keyinfo(1)+reserved(1)+GTK(16)
        v[2..5].copy_from_slice(&RSN_OUI);
        v[5] = 1; // GTK KDE
        v[6] = key_id | if tx { 0x04 } else { 0 };
        v[7] = 0; // reserved
        v[8..24].copy_from_slice(gtk);
        v
    }

    #[test]
    fn extracts_a_gtk_kde_from_key_data() {
        let gtk_bytes = [0xABu8; 16];
        let g = find_gtk(&kde16(&gtk_bytes, 2, true)).unwrap();
        assert_eq!(g.key_id, 2);
        assert_eq!(g.len, 16);
        assert_eq!(&g.key[..16], &gtk_bytes);

        // A tx=false, key_id 1 variant.
        assert_eq!(find_gtk(&kde16(&gtk_bytes, 1, false)).unwrap().key_id, 1);
    }

    #[test]
    fn skips_a_leading_rsn_ie_before_the_gtk_kde() {
        // AP RSN IE (0x30..) then the GTK KDE; the parser must skip the first.
        let gtk_bytes = [0x77u8; 16];
        let mut kd = [0u8; 22 + 24];
        kd[..22].copy_from_slice(&RSN_IE_WPA2_PSK_CCMP);
        kd[22..].copy_from_slice(&kde16(&gtk_bytes, 1, true));
        let g = find_gtk(&kd).unwrap();
        assert_eq!(&g.key[..16], &gtk_bytes);
    }

    #[test]
    fn no_gtk_kde_returns_none() {
        assert!(find_gtk(&RSN_IE_WPA2_PSK_CCMP).is_none());
        assert!(find_gtk(&[]).is_none());
        // A truncated KDE length.
        assert!(find_gtk(&[0xDD, 0x20, 0x00, 0x0f]).is_none());
    }
}
