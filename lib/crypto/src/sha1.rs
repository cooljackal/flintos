// SPDX-License-Identifier: Apache-2.0

//! SHA-1, from FIPS 180-4.
//!
//! WPA2-PSK still rests on SHA-1: PBKDF2-SHA1 turns the passphrase into the
//! PMK, and the pairwise key is derived with a SHA-1 PRF. It is cryptographic
//! but not chosen for its collision resistance here — HMAC-SHA1 and the KDF
//! rely only on it being a pseudo-random function, which the attacks on SHA-1
//! collisions do not touch. WPA3 moves to SHA-256; this stays for the large
//! installed base of WPA2 networks.
//!
//! Same streaming shape as [`super::sha256`], for the same reason, and the
//! same argument that it can be trusted before hardware: SHA-1 is
//! data-independent, so the only question is arithmetic, and the FIPS vectors
//! in the tests answer it.

use super::block::{Block64, BLOCK};

/// A streaming SHA-1 hasher.
#[derive(Clone)]
pub struct Sha1 {
    state: [u32; 5],
    block: Block64,
}

/// The digest is 160 bits.
pub const DIGEST_LEN: usize = 20;

/// FIPS 180-4 §5.3.1.
const H0: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha1 {
    /// A fresh hasher.
    pub const fn new() -> Self {
        Self {
            state: H0,
            block: Block64::new(),
        }
    }

    /// Absorb `data`. Buffering and padding are shared with SHA-256 in
    /// [`super::block`]; only [`compress`] differs.
    pub fn update(&mut self, data: &[u8]) {
        let state = &mut self.state;
        self.block.absorb(data, |block| compress(state, block));
    }

    /// Finish and return the 20-byte digest.
    pub fn finish(mut self) -> [u8; DIGEST_LEN] {
        let state = &mut self.state;
        self.block.finalize(|block| compress(state, block));

        let mut out = [0u8; DIGEST_LEN];
        for (chunk, word) in out.chunks_exact_mut(4).zip(self.state.iter()) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// One-shot: hash `data`.
    pub fn digest(data: &[u8]) -> [u8; DIGEST_LEN] {
        let mut h = Self::new();
        h.update(data);
        h.finish()
    }
}

/// The compression function, FIPS 180-4 §6.1.2.
fn compress(state: &mut [u32; 5], block: &[u8; BLOCK]) {
    let mut w = [0u32; 80];
    for (i, word) in w[..16].iter_mut().enumerate() {
        let j = i * 4;
        *word = u32::from_be_bytes([block[j], block[j + 1], block[j + 2], block[j + 3]]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }

    let [mut a, mut b, mut c, mut d, mut e] = *state;

    for (i, &word) in w.iter().enumerate() {
        // The round function and constant change every twenty rounds.
        let (f, k) = match i {
            0..=19 => ((b & c) | ((!b) & d), 0x5a827999),
            20..=39 => (b ^ c ^ d, 0x6ed9eba1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
            _ => (b ^ c ^ d, 0xca62c1d6),
        };
        let t = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = t;
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &[u8]) -> [u8; DIGEST_LEN] {
        assert_eq!(s.len(), DIGEST_LEN * 2);
        let mut out = [0u8; DIGEST_LEN];
        for (i, o) in out.iter_mut().enumerate() {
            let hi = (s[i * 2] as char).to_digit(16).unwrap() as u8;
            let lo = (s[i * 2 + 1] as char).to_digit(16).unwrap() as u8;
            *o = (hi << 4) | lo;
        }
        out
    }

    #[test]
    fn the_empty_string() {
        assert_eq!(
            Sha1::digest(b""),
            hex(b"da39a3ee5e6b4b0d3255bfef95601890afd80709")
        );
    }

    #[test]
    fn abc_one_block() {
        assert_eq!(
            Sha1::digest(b"abc"),
            hex(b"a9993e364706816aba3e25717850c26c9cd0d89d")
        );
    }

    #[test]
    fn the_length_that_crosses_a_block_boundary() {
        assert_eq!(
            Sha1::digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            hex(b"84983e441c3bd26ebaae4aa1f95129e5e54670f1")
        );
    }

    #[test]
    fn a_million_a() {
        let mut h = Sha1::new();
        let chunk = [b'a'; 7];
        let mut sent = 0;
        while sent < 1_000_000 {
            let n = 7.min(1_000_000 - sent);
            h.update(&chunk[..n]);
            sent += n;
        }
        assert_eq!(h.finish(), hex(b"34aa973cd4c4daa4f61eeb2bdbad27316534016f"));
    }

    #[test]
    fn streaming_matches_one_shot() {
        let data: [u8; 130] = core::array::from_fn(|i| i as u8);
        for split in 0..data.len() {
            let mut h = Sha1::new();
            h.update(&data[..split]);
            h.update(&data[split..]);
            assert_eq!(h.finish(), Sha1::digest(&data), "split at {split}");
        }
    }
}
