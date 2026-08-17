// SPDX-License-Identifier: Apache-2.0

//! The IEEE 802.11 PRF, over HMAC-SHA1.
//!
//! Where the pairwise transient key comes from. Once the PMK is known — from
//! [`wpa_psk`](crate::wpa_psk) — the 4-way handshake mixes it with both
//! MAC addresses and both nonces through this PRF to produce the PTK, from
//! which the key-encryption key, the key-confirmation key and the temporal
//! (data) key are sliced. `sha1_prf` in the adapter's crypto table.
//!
//! # Byte layout, taken from the reference not from memory
//!
//! The exact input to each HMAC is `label ‖ NUL ‖ data ‖ counter`, where the
//! label's length **includes** its NUL terminator and the counter is a single
//! byte starting at zero and incrementing per 20-byte output block. That is
//! read directly from esp-idf's `sha1-prf.c` (`components/wpa_supplicant`),
//! which is the implementation the blobs on the other side of this were built
//! to interoperate with — so matching it byte for byte is the point, and a
//! guess about the spec's wording is exactly what would not do.

use crate::hmac::hmac;
use crate::Sha1;

/// IEEE 802.11 PRF: fill `out` from `key`, a text `label`, and `data`.
///
/// Each 20-byte block of `out` is `HMAC-SHA1(key, label ‖ NUL ‖ data ‖ i)` for
/// `i = 0, 1, 2, …`; the final block is truncated to fit. This is the general
/// form; [`ptk`] wires in the handshake's specific label and data.
pub fn sha1_prf(key: &[u8], label: &[u8], data: &[u8], out: &mut [u8]) {
    const HLEN: usize = 20;
    let mut counter: u8 = 0;
    let mut pos = 0;

    while pos < out.len() {
        // label ‖ NUL ‖ data ‖ counter, assembled in a fixed buffer. The
        // handshake's label and data are small and bounded (a fixed string,
        // two 6-byte MACs and two 32-byte nonces), so 128 bytes is ample.
        let mut buf = [0u8; 128];
        let label_len = label.len() + 1; // includes the NUL
        let total = label_len + data.len() + 1;
        assert!(total <= buf.len(), "sha1_prf input too long");
        buf[..label.len()].copy_from_slice(label);
        buf[label.len()] = 0; // the NUL
        buf[label_len..label_len + data.len()].copy_from_slice(data);
        buf[label_len + data.len()] = counter;

        let mut block = [0u8; HLEN];
        hmac::<Sha1>(key, &buf[..total], &mut block);

        let take = (out.len() - pos).min(HLEN);
        out[pos..pos + take].copy_from_slice(&block[..take]);
        pos += take;
        counter = counter.wrapping_add(1);
    }
}

/// Derive the PTK for a CCMP association.
///
/// The handshake's fixed label is `"Pairwise key expansion"`, and the data is
/// the two MAC addresses then the two nonces, each pair in ascending byte
/// order — the ordering is what lets both ends derive the same key without
/// agreeing who is "first". `X` is the PTK length in bytes: 48 for CCMP (16
/// KCK ‖ 16 KEK ‖ 16 TK), 64 for TKIP.
pub fn ptk<const X: usize>(
    pmk: &[u8],
    aa: &[u8; 6],   // authenticator (AP) address
    spa: &[u8; 6],  // supplicant (station) address
    anonce: &[u8; 32],
    snonce: &[u8; 32],
) -> [u8; X] {
    let mut data = [0u8; 76]; // 6 + 6 + 32 + 32
    let (min_mac, max_mac) = order(aa, spa);
    data[..6].copy_from_slice(min_mac);
    data[6..12].copy_from_slice(max_mac);
    let (min_n, max_n) = order(anonce, snonce);
    data[12..44].copy_from_slice(min_n);
    data[44..76].copy_from_slice(max_n);

    let mut out = [0u8; X];
    sha1_prf(pmk, b"Pairwise key expansion", &data, &mut out);
    out
}

/// The lexicographically smaller slice first. Used on the public MACs and
/// nonces; not secret, so the branch carries nothing.
fn order<'a, const N: usize>(a: &'a [u8; N], b: &'a [u8; N]) -> (&'a [u8; N], &'a [u8; N]) {
    if a[..] < b[..] {
        (a, b)
    } else {
        (b, a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hmac::hmac_sha1;

    // The composition, tested against the vector-checked HMAC underneath: each
    // 20-byte block of the output must be exactly HMAC-SHA1 of the spec input
    // with the matching counter. This pins the concatenation order, the NUL,
    // the counter increment and the truncation. The byte layout itself is
    // taken from esp-idf's sha1-prf.c (see the module docs), and is anchored
    // end to end against a live AP when the handshake wiring lands.

    fn expected_block(key: &[u8], label: &[u8], data: &[u8], counter: u8) -> [u8; 20] {
        let mut buf = [0u8; 128];
        let ll = label.len() + 1;
        buf[..label.len()].copy_from_slice(label);
        buf[label.len()] = 0;
        buf[ll..ll + data.len()].copy_from_slice(data);
        buf[ll + data.len()] = counter;
        hmac_sha1(key, &buf[..ll + data.len() + 1])
    }

    #[test]
    fn blocks_match_the_underlying_hmac() {
        let key = [0x11u8; 32];
        let label = b"Pairwise key expansion";
        let data = [0x22u8; 76];
        let mut out = [0u8; 48];
        sha1_prf(&key, label, &data, &mut out);

        for (i, chunk) in out.chunks(20).enumerate() {
            let exp = expected_block(&key, label, &data, i as u8);
            assert_eq!(chunk, &exp[..chunk.len()], "block {i}");
        }
    }

    #[test]
    fn truncation_is_exact_for_a_non_multiple_length() {
        // 48 is not a multiple of 20: the third block is truncated to 8 bytes.
        let key = [0xaau8; 16];
        let mut out = [0u8; 48];
        sha1_prf(&key, b"L", b"data", &mut out);
        let b2 = expected_block(&key, b"L", b"data", 2);
        assert_eq!(&out[40..48], &b2[..8]);
    }

    #[test]
    fn ptk_is_symmetric_in_address_and_nonce_order() {
        // Both ends compute the same PTK regardless of which is authenticator:
        // swapping the (aa, anonce) and (spa, snonce) pairs must not change it.
        let pmk = [0x0du8; 32];
        let aa = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
        let spa = [0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb];
        let anonce = [0x01u8; 32];
        let snonce = [0x02u8; 32];
        let one = ptk::<48>(&pmk, &aa, &spa, &anonce, &snonce);
        let two = ptk::<48>(&pmk, &spa, &aa, &snonce, &anonce);
        assert_eq!(one, two);
    }
}
