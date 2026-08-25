// SPDX-License-Identifier: Apache-2.0

//! The 64-byte streaming block buffer and Merkle–Damgård padding shared by the
//! SHA-family hashers.
//!
//! SHA-1 and SHA-256 differ only in their compression function and digest
//! width; the buffering (top up a partial block, run whole blocks straight from
//! the input, keep the remainder) and the FIPS 180-4 padding (a `0x80` byte,
//! zeros, then the 64-bit big-endian bit length, with the awkward case where
//! the suffix spills into a second block) are byte-for-byte identical. They
//! lived in both files — and a third copy in the hardware driver — so a fix to
//! the spill edge had to land three times. Here they live once; each hasher
//! passes its own compression function as a closure.

/// SHA-1/SHA-256 block size in bytes (512 bits).
pub const BLOCK: usize = 64;

/// The length suffix is 8 bytes (a 64-bit big-endian bit count).
const LEN_SUFFIX: usize = 8;

/// A streaming 64-byte block buffer with SHA-family padding.
#[derive(Clone)]
pub(crate) struct Block64 {
    /// Bytes not yet absorbed into a full block.
    buf: [u8; BLOCK],
    /// How many of `buf` are filled.
    buf_len: usize,
    /// Total message length in bytes, for the length suffix. 64-bit as the
    /// standard requires.
    len: u64,
}

impl Block64 {
    pub const fn new() -> Self {
        Self {
            buf: [0u8; BLOCK],
            buf_len: 0,
            len: 0,
        }
    }

    /// Absorb `data`, invoking `compress` once for each full block — from the
    /// buffer when a partial block fills, then straight from the input with no
    /// copy, keeping any trailing partial block for next time.
    pub fn absorb(&mut self, mut data: &[u8], mut compress: impl FnMut(&[u8; BLOCK])) {
        self.len = self.len.wrapping_add(data.len() as u64);

        if self.buf_len > 0 {
            let need = BLOCK - self.buf_len;
            let take = need.min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == BLOCK {
                compress(&self.buf);
                self.buf_len = 0;
            }
        }

        while data.len() >= BLOCK {
            let (block, rest) = data.split_at(BLOCK);
            compress(block.try_into().unwrap());
            data = rest;
        }

        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    /// Append the FIPS 180-4 padding — a single `0x80`, zeros, then the 64-bit
    /// big-endian bit length — and compress the final block(s). When the suffix
    /// will not fit in the current block, pad it out and compress it first.
    pub fn finalize(&mut self, mut compress: impl FnMut(&[u8; BLOCK])) {
        let bit_len = self.len.wrapping_mul(8);

        // Always fits: buf_len < BLOCK on entry.
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;

        if self.buf_len > BLOCK - LEN_SUFFIX {
            for b in &mut self.buf[self.buf_len..] {
                *b = 0;
            }
            compress(&self.buf);
            self.buf_len = 0;
        }

        for b in &mut self.buf[self.buf_len..BLOCK - LEN_SUFFIX] {
            *b = 0;
        }
        self.buf[BLOCK - LEN_SUFFIX..].copy_from_slice(&bit_len.to_be_bytes());
        compress(&self.buf);
    }
}
