// SPDX-License-Identifier: Apache-2.0

//! SHA-256, from FIPS 180-4.
//!
//! The workhorse of WPA2/WPA3: the PRF that derives the pairwise key, the
//! HMAC that authenticates each handshake message, and the PMKID all reduce to
//! SHA-256. It is written as a streaming [`Sha256`] rather than a one-shot,
//! because the supplicant hashes several non-contiguous pieces — a nonce, two
//! MAC addresses, a label — and copying them into one buffer first would be
//! both slower and an extra place for a length bug to hide.
//!
//! # Why this can be trusted before hardware
//!
//! SHA-256 is entirely data-independent: the same sequence of operations runs
//! for every input of a given length, so there is no timing side channel to
//! reason about and no branch that depends on a secret. What remains is
//! whether the arithmetic is right, and that is what the FIPS vectors in the
//! tests settle — the empty string, the one-block "abc", the two-block case
//! that exercises the length padding across a boundary, and a millon-'a'
//! case that exercises many blocks.

/// A streaming SHA-256 hasher.
///
/// Feed it with [`update`](Sha256::update) as many times as needed, then take
/// the digest with [`finish`](Sha256::finish). The value is consumed, because
/// the padding makes finishing a one-way step: there is no meaningful state
/// to keep afterwards.
#[derive(Clone)]
pub struct Sha256 {
    /// The eight working hash words, `H0..H7`.
    state: [u32; 8],
    /// Total message length in bytes, for the length suffix. 64-bit as the
    /// standard requires; a `usize` would cap a hash at 4 GiB on this target,
    /// which no handshake reaches but the format still specifies.
    len: u64,
    /// Bytes not yet absorbed into a full 64-byte block.
    buf: [u8; BLOCK],
    /// How many of `buf` are filled.
    buf_len: usize,
}

/// SHA-256 processes the message in 512-bit (64-byte) blocks.
const BLOCK: usize = 64;

/// The digest is 256 bits.
pub const DIGEST_LEN: usize = 32;

/// FIPS 180-4 §4.2.2: the first 32 bits of the fractional parts of the cube
/// roots of the first 64 primes.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// FIPS 180-4 §5.3.3: the first 32 bits of the fractional parts of the square
/// roots of the first 8 primes.
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// A fresh hasher, primed with the standard initial state.
    pub const fn new() -> Self {
        Self {
            state: H0,
            len: 0,
            buf: [0u8; BLOCK],
            buf_len: 0,
        }
    }

    /// Absorb `data`. May be called any number of times; the result is as if
    /// every `data` had been concatenated.
    pub fn update(&mut self, mut data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);

        // Top up a partial block first, and process it once full.
        if self.buf_len > 0 {
            let need = BLOCK - self.buf_len;
            let take = need.min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == BLOCK {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }

        // Whole blocks straight from the input, no copy.
        while data.len() >= BLOCK {
            let (block, rest) = data.split_at(BLOCK);
            self.compress(block.try_into().unwrap());
            data = rest;
        }

        // Whatever is left is a partial block for next time.
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    /// Finish and return the 32-byte digest.
    ///
    /// Appends the padding the standard requires: a single `0x80` byte, then
    /// zeros, then the 64-bit big-endian bit length, arranged so the whole
    /// message is a multiple of 64 bytes.
    pub fn finish(mut self) -> [u8; DIGEST_LEN] {
        let bit_len = self.len.wrapping_mul(8);

        // The 0x80 always fits: buf_len < BLOCK on entry.
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;

        // If the length suffix will not fit in this block, pad this one out
        // with zeros, compress it, and start a fresh block for the suffix.
        if self.buf_len > BLOCK - 8 {
            for b in &mut self.buf[self.buf_len..] {
                *b = 0;
            }
            let block = self.buf;
            self.compress(&block);
            self.buf_len = 0;
        }

        for b in &mut self.buf[self.buf_len..BLOCK - 8] {
            *b = 0;
        }
        self.buf[BLOCK - 8..].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        self.compress(&block);

        let mut out = [0u8; DIGEST_LEN];
        for (chunk, word) in out.chunks_exact_mut(4).zip(self.state.iter()) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// One-shot convenience: hash `data` and return the digest.
    pub fn digest(data: &[u8]) -> [u8; DIGEST_LEN] {
        let mut h = Self::new();
        h.update(data);
        h.finish()
    }

    /// The compression function, FIPS 180-4 §6.2.2, over one 64-byte block.
    fn compress(&mut self, block: &[u8; BLOCK]) {
        // Message schedule. The first sixteen words are the block, big-endian;
        // the rest are derived.
        let mut w = [0u32; 64];
        for (i, word) in w[..16].iter_mut().enumerate() {
            let j = i * 4;
            *word = u32::from_be_bytes([block[j], block[j + 1], block[j + 2], block[j + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> [u8; DIGEST_LEN] {
        // The tests carry expected digests as hex string literals; this turns
        // one into bytes so the assertion compares arrays, not formatting.
        let s = bytes;
        assert_eq!(s.len(), DIGEST_LEN * 2);
        let mut out = [0u8; DIGEST_LEN];
        for (i, o) in out.iter_mut().enumerate() {
            let hi = (s[i * 2] as char).to_digit(16).unwrap() as u8;
            let lo = (s[i * 2 + 1] as char).to_digit(16).unwrap() as u8;
            *o = (hi << 4) | lo;
        }
        out
    }

    // FIPS 180-4 and the standard published test vectors.

    #[test]
    fn the_empty_string() {
        assert_eq!(
            Sha256::digest(b""),
            hex(b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
    }

    #[test]
    fn abc_one_block() {
        assert_eq!(
            Sha256::digest(b"abc"),
            hex(b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn the_length_that_crosses_a_block_boundary() {
        // 56 bytes: one byte short of forcing the length suffix into a second
        // block, which is exactly the padding edge case worth pinning.
        assert_eq!(
            Sha256::digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            hex(b"248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1")
        );
    }

    #[test]
    fn a_million_a() {
        // Many blocks, and a length past a single byte. Fed in awkward chunk
        // sizes to prove the streaming buffer handles arbitrary splits.
        let mut h = Sha256::new();
        let chunk = [b'a'; 7];
        let mut sent = 0;
        while sent < 1_000_000 {
            let n = 7.min(1_000_000 - sent);
            h.update(&chunk[..n]);
            sent += n;
        }
        assert_eq!(
            h.finish(),
            hex(b"cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0")
        );
    }

    #[test]
    fn streaming_matches_one_shot() {
        // Byte-at-a-time must equal the whole-slice digest, for every prefix
        // length across a block boundary.
        let data: [u8; 130] = core::array::from_fn(|i| i as u8);
        for split in 0..data.len() {
            let mut h = Sha256::new();
            h.update(&data[..split]);
            h.update(&data[split..]);
            assert_eq!(h.finish(), Sha256::digest(&data), "split at {split}");
        }
    }
}
