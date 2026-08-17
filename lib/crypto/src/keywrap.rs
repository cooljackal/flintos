// SPDX-License-Identifier: Apache-2.0

//! AES Key Wrap, from RFC 3394.
//!
//! How the group key reaches a station: handshake message 3 carries the GTK
//! wrapped under the KEK (a slice of the pairwise key), and the station
//! unwraps it. `aes_wrap`/`aes_unwrap` in the crypto table. The unwrap also
//! **authenticates** — it recomputes the integrity value the wrap embedded and
//! reports a mismatch rather than handing back garbage, so a tampered or
//! wrong-key message is a clean failure, not a silently corrupt GTK.
//!
//! Works over 64-bit half-blocks with the default RFC 3394 integrity constant.
//! The KEK is 128 or 256 bits; both are here because WPA2 uses a 128-bit KEK
//! and WPA3 a 256-bit one.

use crate::aes::{Aes128, Aes256};

/// RFC 3394 §2.2.3.1: the default initial integrity value.
const DEFAULT_IV: [u8; 8] = [0xa6; 8];

/// Errors from [`aes_unwrap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnwrapError {
    /// The wrapped input was not a whole number of 64-bit blocks, or too short.
    BadLength,
    /// The integrity check failed: wrong key, or the ciphertext was altered.
    /// The output is not written.
    Integrity,
}

/// An expanded KEK, either size, presented as a single block operation.
enum Kek {
    A128(Aes128),
    A256(Aes256),
}

impl Kek {
    fn from(kek: &[u8]) -> Option<Self> {
        match kek.len() {
            16 => Some(Kek::A128(Aes128::new(kek.try_into().unwrap()))),
            32 => Some(Kek::A256(Aes256::new(kek.try_into().unwrap()))),
            _ => None,
        }
    }
    fn encrypt(&self, b: &mut [u8; 16]) {
        match self {
            Kek::A128(c) => c.encrypt_block(b),
            Kek::A256(c) => c.encrypt_block(b),
        }
    }
    fn decrypt(&self, b: &mut [u8; 16]) {
        match self {
            Kek::A128(c) => c.decrypt_block(b),
            Kek::A256(c) => c.decrypt_block(b),
        }
    }
}

/// Wrap `plaintext` (a whole number of 8-byte blocks, at least one) under
/// `kek`, writing `plaintext.len() + 8` bytes into `out`. Returns the number
/// of bytes written, or `None` if the sizes are wrong.
pub fn aes_wrap(kek: &[u8], plaintext: &[u8], out: &mut [u8]) -> Option<usize> {
    let n = plaintext.len() / 8;
    if plaintext.is_empty() || plaintext.len() % 8 != 0 || out.len() < plaintext.len() + 8 {
        return None;
    }
    let cipher = Kek::from(kek)?;

    let mut a = DEFAULT_IV;
    // R[1..=n] hold the data half-blocks; keep them in `out[8..]` in place.
    out[8..8 + plaintext.len()].copy_from_slice(plaintext);

    for j in 0..6u64 {
        for i in 1..=n {
            let mut block = [0u8; 16];
            block[..8].copy_from_slice(&a);
            block[8..].copy_from_slice(&out[8 * i..8 * i + 8]);
            cipher.encrypt(&mut block);
            a.copy_from_slice(&block[..8]);
            // A ^= t, where t = n*j + i as a 64-bit big-endian counter.
            let t = n as u64 * j + i as u64;
            for (k, tb) in t.to_be_bytes().iter().enumerate() {
                a[k] ^= *tb;
            }
            out[8 * i..8 * i + 8].copy_from_slice(&block[8..]);
        }
    }

    out[..8].copy_from_slice(&a);
    Some(plaintext.len() + 8)
}

/// Unwrap `wrapped` under `kek`, writing `wrapped.len() - 8` plaintext bytes
/// into `out`. Verifies the integrity value; on mismatch nothing usable is
/// left in `out` and [`UnwrapError::Integrity`] is returned.
pub fn aes_unwrap(kek: &[u8], wrapped: &[u8], out: &mut [u8]) -> Result<usize, UnwrapError> {
    if wrapped.len() < 16 || wrapped.len() % 8 != 0 {
        return Err(UnwrapError::BadLength);
    }
    let n = wrapped.len() / 8 - 1;
    if out.len() < n * 8 {
        return Err(UnwrapError::BadLength);
    }
    let cipher = Kek::from(kek).ok_or(UnwrapError::BadLength)?;

    let mut a = [0u8; 8];
    a.copy_from_slice(&wrapped[..8]);
    out[..n * 8].copy_from_slice(&wrapped[8..]);

    for j in (0..6u64).rev() {
        for i in (1..=n).rev() {
            let t = n as u64 * j + i as u64;
            let mut block = [0u8; 16];
            for (k, tb) in t.to_be_bytes().iter().enumerate() {
                block[k] = a[k] ^ *tb;
            }
            block[8..].copy_from_slice(&out[8 * (i - 1)..8 * (i - 1) + 8]);
            cipher.decrypt(&mut block);
            a.copy_from_slice(&block[..8]);
            out[8 * (i - 1)..8 * (i - 1) + 8].copy_from_slice(&block[8..]);
        }
    }

    // Constant-time-ish compare against the integrity constant. The value is
    // not secret, but a short-circuit here would leak how much matched.
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(DEFAULT_IV.iter()) {
        diff |= x ^ y;
    }
    if diff != 0 {
        // Do not leave the (unverified) plaintext where a caller might use it.
        for b in out[..n * 8].iter_mut() {
            *b = 0;
        }
        return Err(UnwrapError::Integrity);
    }
    Ok(n * 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hx(s: &[u8], out: &mut [u8]) {
        for i in 0..out.len() {
            let hi = (s[i * 2] as char).to_digit(16).unwrap() as u8;
            let lo = (s[i * 2 + 1] as char).to_digit(16).unwrap() as u8;
            out[i] = (hi << 4) | lo;
        }
    }

    #[test]
    fn rfc3394_128kek_128data() {
        let mut kek = [0u8; 16];
        hx(b"000102030405060708090a0b0c0d0e0f", &mut kek);
        let mut data = [0u8; 16];
        hx(b"00112233445566778899aabbccddeeff", &mut data);
        let mut expected = [0u8; 24];
        hx(
            b"1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5",
            &mut expected,
        );

        let mut wrapped = [0u8; 24];
        assert_eq!(aes_wrap(&kek, &data, &mut wrapped), Some(24));
        assert_eq!(wrapped, expected);

        let mut back = [0u8; 16];
        assert_eq!(aes_unwrap(&kek, &wrapped, &mut back), Ok(16));
        assert_eq!(back, data);
    }

    #[test]
    fn rfc3394_256kek_256data() {
        let mut kek = [0u8; 32];
        hx(
            b"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            &mut kek,
        );
        let mut data = [0u8; 32];
        hx(
            b"00112233445566778899aabbccddeeff000102030405060708090a0b0c0d0e0f",
            &mut data,
        );
        let mut expected = [0u8; 40];
        hx(
            b"28c9f404c4b810f4cbccb35cfb87f8263f5786e2d80ed326cbc7f0e71a99f43bfb988b9b7a02dd21",
            &mut expected,
        );

        let mut wrapped = [0u8; 40];
        assert_eq!(aes_wrap(&kek, &data, &mut wrapped), Some(40));
        assert_eq!(wrapped, expected);

        let mut back = [0u8; 32];
        assert_eq!(aes_unwrap(&kek, &wrapped, &mut back), Ok(32));
        assert_eq!(back, data);
    }

    #[test]
    fn a_tampered_wrap_fails_integrity_and_zeroes_output() {
        let mut kek = [0u8; 16];
        hx(b"000102030405060708090a0b0c0d0e0f", &mut kek);
        let mut data = [0u8; 16];
        hx(b"00112233445566778899aabbccddeeff", &mut data);
        let mut wrapped = [0u8; 24];
        aes_wrap(&kek, &data, &mut wrapped);
        wrapped[20] ^= 0x01; // flip a bit in the ciphertext

        let mut back = [0xffu8; 16];
        assert_eq!(
            aes_unwrap(&kek, &wrapped, &mut back),
            Err(UnwrapError::Integrity)
        );
        assert_eq!(back, [0u8; 16], "unverified plaintext must not be left behind");
    }
}
