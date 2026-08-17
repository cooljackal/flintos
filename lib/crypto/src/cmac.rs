// SPDX-License-Identifier: Apache-2.0

//! AES-CMAC, from RFC 4493 (a.k.a. OMAC1).
//!
//! The message integrity check on WPA2 EAPOL-Key frames that use the
//! AES-128-CMAC AKM, and `omac1_aes_128` in the adapter's crypto table. It is
//! a CBC-MAC with a pair of derived subkeys that remove the length-extension
//! weakness plain CBC-MAC has — the subkey applied to the final block differs
//! depending on whether that block was full or padded, so two messages where
//! one is a prefix of the other cannot share a tag.
//!
//! Built entirely on [`Aes128`]; no new field arithmetic beyond the subkey
//! doubling, which is a single multiply by x in GF(2^128).

use crate::aes::Aes128;

/// The GF(2^128) reduction byte for the subkey doubling (RFC 4493 §2.3).
const RB: u8 = 0x87;

/// Multiply a 16-byte big-endian value by x in GF(2^128).
///
/// A left shift by one bit across the whole block; if the top bit was set, the
/// reduction polynomial is folded back in via the low byte. Used only on the
/// public subkeys, so its timing carries nothing secret.
fn dbl(block: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    let mut carry = 0u8;
    for i in (0..16).rev() {
        let b = block[i];
        out[i] = (b << 1) | carry;
        carry = b >> 7;
    }
    if block[0] & 0x80 != 0 {
        out[15] ^= RB;
    }
    out
}

/// AES-128-CMAC of `msg` under `key`. Sixteen-byte tag.
pub fn aes_cmac(key: &[u8; 16], msg: &[u8]) -> [u8; 16] {
    let cipher = Aes128::new(key);

    // Subkeys: L = AES(0), K1 = L·x, K2 = K1·x.
    let mut l = [0u8; 16];
    cipher.encrypt_block(&mut l);
    let k1 = dbl(&l);
    let k2 = dbl(&k1);

    // The final block gets K1 if the message is a non-empty multiple of the
    // block size, K2 (with 10* padding) otherwise.
    let n = msg.len();
    let complete = n != 0 && n % 16 == 0;
    let last_block = {
        let mut b = [0u8; 16];
        if complete {
            b.copy_from_slice(&msg[n - 16..]);
            for (bb, kk) in b.iter_mut().zip(k1.iter()) {
                *bb ^= *kk;
            }
        } else {
            let start = n - (n % 16);
            let rem = &msg[start..];
            b[..rem.len()].copy_from_slice(rem);
            b[rem.len()] = 0x80;
            for (bb, kk) in b.iter_mut().zip(k2.iter()) {
                *bb ^= *kk;
            }
        }
        b
    };

    // CBC-MAC over all but the last block, then the adjusted last block.
    let mut x = [0u8; 16];
    let full_blocks = if complete { n / 16 - 1 } else { n / 16 };
    for i in 0..full_blocks {
        for j in 0..16 {
            x[j] ^= msg[i * 16 + j];
        }
        cipher.encrypt_block(&mut x);
    }
    for j in 0..16 {
        x[j] ^= last_block[j];
    }
    cipher.encrypt_block(&mut x);
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hx(s: &[u8]) -> [u8; 16] {
        let mut out = [0u8; 16];
        for (i, o) in out.iter_mut().enumerate() {
            let hi = (s[i * 2] as char).to_digit(16).unwrap() as u8;
            let lo = (s[i * 2 + 1] as char).to_digit(16).unwrap() as u8;
            *o = (hi << 4) | lo;
        }
        out
    }

    // RFC 4493 §4: the four AES-128-CMAC test vectors, key 2b7e1516...
    const KEY: &[u8; 32] = b"2b7e151628aed2a6abf7158809cf4f3c";
    // The RFC's example message, 64 bytes, of which the cases use prefixes.
    const MSG: &[u8; 128] = b"6bc1bee22e409f96e93d7e117393172a\
ae2d8a571e03ac9c9eb76fac45af8e51\
30c81c46a35ce411e5fbc1191a0a52ef\
f69f2445df4f9b17ad2b417be66c3710";

    fn key() -> [u8; 16] {
        hx(KEY)
    }

    fn msg_bytes(n: usize) -> [u8; 64] {
        // Decode the hex message once, take the first n bytes.
        let mut full = [0u8; 64];
        for (i, o) in full.iter_mut().enumerate() {
            let hi = (MSG[i * 2] as char).to_digit(16).unwrap() as u8;
            let lo = (MSG[i * 2 + 1] as char).to_digit(16).unwrap() as u8;
            *o = (hi << 4) | lo;
        }
        let mut out = [0u8; 64];
        out[..n].copy_from_slice(&full[..n]);
        out
    }

    #[test]
    fn rfc4493_empty() {
        assert_eq!(
            aes_cmac(&key(), b""),
            hx(b"bb1d6929e95937287fa37d129b756746")
        );
    }

    #[test]
    fn rfc4493_one_block() {
        assert_eq!(
            aes_cmac(&key(), &msg_bytes(16)[..16]),
            hx(b"070a16b46b4d4144f79bdd9dd04a287c")
        );
    }

    #[test]
    fn rfc4493_partial_last_block() {
        // 40 bytes: last block is padded, so K2 is used.
        assert_eq!(
            aes_cmac(&key(), &msg_bytes(40)[..40]),
            hx(b"dfa66747de9ae63030ca32611497c827")
        );
    }

    #[test]
    fn rfc4493_four_full_blocks() {
        // 64 bytes: complete, so K1 on the last block.
        assert_eq!(
            aes_cmac(&key(), &msg_bytes(64)[..64]),
            hx(b"51f0bebf7e3b9d92fc49741779363cfe")
        );
    }
}
