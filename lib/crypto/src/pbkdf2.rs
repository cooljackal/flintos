// SPDX-License-Identifier: Apache-2.0

//! PBKDF2-HMAC-SHA1, from RFC 2898, and the WPA passphrase mapping on top.
//!
//! This is the first thing that happens when FlintOS joins a WPA2-PSK
//! network: the passphrase and the SSID become the 256-bit pairwise master
//! key. IEEE 802.11i fixes the parameters — HMAC-SHA1, 4096 iterations, the
//! SSID as salt, a 32-byte output — so [`wpa_psk`] wires those in and the
//! caller supplies only the two secrets.
//!
//! It is deliberately, specified-ly slow: 4096 iterations is the cost that
//! makes a passphrase guess expensive. On the ESP32 that is a few hundred
//! milliseconds, paid once per association, and the SHA accelerator (#33) is
//! what brings it down to esp-idf's timing.

use crate::hmac::hmac_sha1;

/// PBKDF2 with HMAC-SHA1 as the PRF (RFC 2898 §5.2).
///
/// Fills `out` with as many derived bytes as it is long, running `iterations`
/// of HMAC-SHA1 per 20-byte output block. `iterations` must be at least 1.
pub fn pbkdf2_sha1(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    assert!(iterations >= 1);
    const HLEN: usize = 20;

    // Each output block Ti covers HLEN bytes and is F(password, salt, c, i).
    for (block_index, chunk) in out.chunks_mut(HLEN).enumerate() {
        let i = (block_index as u32) + 1;

        // U1 = PRF(password, salt || INT_BE(i))
        let (salt_i, salt_i_len) = concat_salt_index(salt, i);
        let mut u = hmac_sha1(password, &salt_i[..salt_i_len]);

        // T = U1, then XOR in U2..Uc, each U = PRF(password, previous U).
        let mut t = u;
        for _ in 1..iterations {
            u = hmac_sha1(password, &u);
            for (t_b, u_b) in t.iter_mut().zip(u.iter()) {
                *t_b ^= *u_b;
            }
        }

        let n = chunk.len().min(HLEN);
        chunk[..n].copy_from_slice(&t[..n]);
    }
}

/// The 802.11i passphrase mapping: PMK = PBKDF2(passphrase, ssid, 4096, 256).
///
/// The one call the supplicant makes to turn what the user typed into the key
/// the handshake proves knowledge of. SSID is the salt exactly as given —
/// bytes, not a string, because an SSID is not required to be UTF-8.
pub fn wpa_psk(passphrase: &[u8], ssid: &[u8]) -> [u8; 32] {
    let mut pmk = [0u8; 32];
    pbkdf2_sha1(passphrase, ssid, 4096, &mut pmk);
    pmk
}

/// `salt || INT_BE(index)` in a fixed buffer, returning the buffer and its
/// used length. The salt (an SSID) is at most 32 bytes and the index is 4, so
/// a 64-byte buffer is ample and avoids an allocation on the join path.
fn concat_salt_index(salt: &[u8], index: u32) -> ([u8; 64], usize) {
    let mut buf = [0u8; 64];
    let suffix = index.to_be_bytes();
    let n = salt.len() + suffix.len();
    assert!(n <= buf.len(), "PBKDF2 salt too long");
    buf[..salt.len()].copy_from_slice(salt);
    buf[salt.len()..n].copy_from_slice(&suffix);
    (buf, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hx20(s: &[u8]) -> [u8; 20] {
        let mut out = [0u8; 20];
        for (i, o) in out.iter_mut().enumerate() {
            let hi = (s[i * 2] as char).to_digit(16).unwrap() as u8;
            let lo = (s[i * 2 + 1] as char).to_digit(16).unwrap() as u8;
            *o = (hi << 4) | lo;
        }
        out
    }

    // RFC 6070: PBKDF2-HMAC-SHA1 test vectors.

    #[test]
    fn rfc6070_one_iteration() {
        let mut out = [0u8; 20];
        pbkdf2_sha1(b"password", b"salt", 1, &mut out);
        assert_eq!(out, hx20(b"0c60c80f961f0e71f3a9b524af6012062fe037a6"));
    }

    #[test]
    fn rfc6070_two_iterations() {
        let mut out = [0u8; 20];
        pbkdf2_sha1(b"password", b"salt", 2, &mut out);
        assert_eq!(out, hx20(b"ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957"));
    }

    #[test]
    fn rfc6070_4096_iterations() {
        let mut out = [0u8; 20];
        pbkdf2_sha1(b"password", b"salt", 4096, &mut out);
        assert_eq!(out, hx20(b"4b007901b765489abead49d926f721d065a429c1"));
    }

    #[test]
    fn rfc6070_longer_output_spans_blocks() {
        // 25 bytes: two HMAC blocks, the second truncated -- the case that a
        // per-block length bug would miss.
        let mut out = [0u8; 25];
        pbkdf2_sha1(
            b"passwordPASSWORDpassword",
            b"saltSALTsaltSALTsaltSALTsaltSALTsalt",
            4096,
            &mut out,
        );
        let expect = [
            0x3d, 0x2e, 0xec, 0x4f, 0xe4, 0x1c, 0x84, 0x9b, 0x80, 0xc8, 0xd8, 0x36, 0x62,
            0xc0, 0xe4, 0x4a, 0x8b, 0x29, 0x1a, 0x96, 0x4c, 0xf2, 0xf0, 0x70, 0x38,
        ];
        assert_eq!(out, expect);
    }

    #[test]
    fn ieee80211_wpa_psk_vector() {
        // IEEE 802.11i-2004 Annex H.4.2: passphrase "password", SSID "IEEE".
        let pmk = wpa_psk(b"password", b"IEEE");
        let expect = [
            0xf4, 0x2c, 0x6f, 0xc5, 0x2d, 0xf0, 0xeb, 0xef, 0x9e, 0xbb, 0x4b, 0x90, 0xb3,
            0x8a, 0x5f, 0x90, 0x2e, 0x83, 0xfe, 0x1b, 0x13, 0x5a, 0x70, 0xe2, 0x3a, 0xed,
            0x76, 0x2e, 0x97, 0x10, 0xa1, 0x2e,
        ];
        assert_eq!(pmk, expect);
    }
}
