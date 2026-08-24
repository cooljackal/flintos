// SPDX-License-Identifier: Apache-2.0

//! The ESP32 SHA accelerator: one-shot SHA-1 and SHA-256 over a byte slice.
//!
//! The hardware hashes 512-bit blocks and nothing else. It does **not** pad
//! the message and it does **not** length-append — those are the caller's job,
//! and `pad_tail` does them here, in software, where they can be tested. The
//! block engine only ever sees whole 64-byte blocks.
//!
//! # Sequence (esp-idf `sha_hal.c` / `sha_ll.h`)
//!
//! For each block: wait until not busy, write the 16 words, then pulse
//! `START` for the first block or `CONTINUE` for every one after. The hardware
//! keeps the running state in its own registers between blocks; `START` versus
//! `CONTINUE` is the only thing that tells it whether to begin from the SHA
//! initial constants or fold into the state already there. After the last
//! block: pulse `LOAD`, wait idle, then read the digest out of the same text
//! registers.
//!
//! # Byte order
//!
//! The message words go in **big-endian**: esp-idf writes `HAL_SWAP32(word)`
//! of a little-endian load, which is exactly `u32::from_be_bytes` of the four
//! message bytes. The digest comes back the same way — each register word is a
//! big-endian slice of the output — so we write it out with `to_be_bytes`.
//! This is the SHA half of the endianness gotcha in the crate docs, and it is
//! the *opposite* of AES.

use soc_esp32::addr::SHA_BASE;
use soc_esp32::dport::{self, CryptoClockBit};
use soc_esp32::poll;

use crate::CryptoError;

/// Which SHA algorithm. The value is the register-bank offset multiplier the
/// hardware uses (`SHA_1_*` at `+0x80`, `SHA_256_*` at `+0x90`), so the
/// per-type register addresses fall straight out of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaKind {
    /// SHA-1, 20-byte digest.
    Sha1,
    /// SHA-256, 32-byte digest.
    Sha256,
}

impl ShaKind {
    /// Digest length in bytes.
    const fn digest_len(self) -> usize {
        match self {
            ShaKind::Sha1 => 20,
            ShaKind::Sha256 => 32,
        }
    }

    /// Base of this type's `START`/`CONTINUE`/`LOAD`/`BUSY` bank.
    ///
    /// `SHA_START_REG(type) = SHA_1_START_REG + type * 0x10` in esp-idf;
    /// SHA-1 is type 0 (`+0x80`), SHA-256 is type 1 (`+0x90`).
    const fn bank(self) -> u32 {
        match self {
            ShaKind::Sha1 => SHA_BASE + 0x80,
            ShaKind::Sha256 => SHA_BASE + 0x90,
        }
    }
}

/// The 16-word message/digest register file, shared by every SHA type.
const SHA_TEXT: u32 = SHA_BASE; // + 0x00
const START_OFF: u32 = 0x00;
const CONTINUE_OFF: u32 = 0x04;
const LOAD_OFF: u32 = 0x08;
const BUSY_OFF: u32 = 0x0C;

/// Bytes in a 512-bit block.
const BLOCK_BYTES: usize = 64;

/// SHA-1 of `data`.
///
/// # Safety
/// Takes the SHA accelerator for the duration and gates its clock on and off.
/// Do not call concurrently with another SHA user; the block has one state.
pub unsafe fn sha1(data: &[u8]) -> Result<[u8; 20], CryptoError> {
    let mut out = [0u8; 20];
    digest(ShaKind::Sha1, data, &mut out)?;
    Ok(out)
}

/// SHA-256 of `data`.
///
/// # Safety
/// As [`sha1`].
pub unsafe fn sha256(data: &[u8]) -> Result<[u8; 32], CryptoError> {
    let mut out = [0u8; 32];
    digest(ShaKind::Sha256, data, &mut out)?;
    Ok(out)
}

/// Hash `data` and write the digest into `out`, which must be exactly
/// `kind.digest_len()` long.
///
/// # Safety
/// See [`sha1`].
unsafe fn digest(kind: ShaKind, data: &[u8], out: &mut [u8]) -> Result<(), CryptoError> {
    debug_assert_eq!(out.len(), kind.digest_len());

    dport::enable_crypto(CryptoClockBit::SHA);

    // The message split into whole 64-byte blocks plus a short remainder. The
    // remainder is padded into one or two tail blocks below.
    let mut chunks = data.chunks_exact(BLOCK_BYTES);
    let mut first = true;
    for block in &mut chunks {
        feed(kind, block, first)?;
        first = false;
    }

    // Padding: 0x80, zero fill, then the 64-bit big-endian bit length. This is
    // the part hand-rolled SHA drivers get wrong, so it is a pure function
    // (`pad_tail`) with its own host tests.
    let mut tail = [0u8; 2 * BLOCK_BYTES];
    let tail_blocks = pad_tail(chunks.remainder(), data.len() as u64, &mut tail);
    for i in 0..tail_blocks {
        feed(kind, &tail[i * BLOCK_BYTES..(i + 1) * BLOCK_BYTES], first)?;
        first = false;
    }

    // The last block's START/CONTINUE was pulsed but not waited on; the engine
    // must finish it before LOAD, or LOAD latches a half-computed (zero) state.
    wait_idle(kind)?;
    // Latch the digest into the text registers, then read it out big-endian
    // (each word's bytes reversed back to the canonical digest order).
    dport::write(kind.bank() + LOAD_OFF, 1);
    wait_idle(kind)?;
    for (i, word) in out.chunks_exact_mut(4).enumerate() {
        let v = dport::read(SHA_TEXT + (i as u32) * 4);
        word.copy_from_slice(&v.to_be_bytes());
    }

    dport::disable_crypto(CryptoClockBit::SHA);
    Ok(())
}

/// Push one 64-byte block through the engine.
///
/// # Safety
/// The SHA clock must be on and `block` must be exactly 64 bytes.
unsafe fn feed(kind: ShaKind, block: &[u8], first: bool) -> Result<(), CryptoError> {
    wait_idle(kind)?;
    for (i, word) in block.chunks_exact(4).enumerate() {
        // Big-endian: the message byte stream, word by word.
        let v = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        dport::write(SHA_TEXT + (i as u32) * 4, v);
    }
    let off = if first { START_OFF } else { CONTINUE_OFF };
    dport::write(kind.bank() + off, 1);
    Ok(())
}

/// Spin until this SHA type's `BUSY` register reads zero.
///
/// # Safety
/// The SHA clock must be on.
unsafe fn wait_idle(kind: ShaKind) -> Result<(), CryptoError> {
    poll::until_us(poll::DEFAULT_TIMEOUT_US, || dport::read(kind.bank() + BUSY_OFF) == 0)
        .map_err(|_| CryptoError::Timeout)
}

/// Build the SHA padding tail from the message remainder.
///
/// Appends the `0x80` marker byte, zero-fills, and writes the message length
/// in bits as a 64-bit big-endian integer in the last 8 bytes. Returns the
/// number of 64-byte blocks written into `out` (1 when the remainder leaves
/// room for the length field, 2 when it does not).
///
/// `rem` is the trailing bytes of the message that did not fill a whole block,
/// so `rem.len() < 64`. `total_len` is the whole message length in bytes.
/// `out` must be at least 128 bytes.
///
/// Pure: no register touched, which is the point — the off-by-one that eats
/// the length field or the case where padding spills a second block are both
/// checkable on a host.
fn pad_tail(rem: &[u8], total_len: u64, out: &mut [u8]) -> usize {
    debug_assert!(rem.len() < BLOCK_BYTES);
    let n = rem.len();

    // A second block is needed only when the 0x80 marker leaves fewer than 8
    // bytes for the length field — i.e. the remainder is 56..=63 bytes.
    let blocks = if n >= BLOCK_BYTES - 8 { 2 } else { 1 };
    let span = blocks * BLOCK_BYTES;

    for b in out[..span].iter_mut() {
        *b = 0;
    }
    out[..n].copy_from_slice(rem);
    out[n] = 0x80;
    let bit_len = total_len.wrapping_mul(8);
    out[span - 8..span].copy_from_slice(&bit_len.to_be_bytes());
    blocks
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_banks_match_esp_idf() {
        // hwcrypto_reg.h: SHA_1_START at DR_REG_SHA_BASE+0x80, SHA_256 at +0x90.
        assert_eq!(ShaKind::Sha1.bank(), 0x3FF0_3080);
        assert_eq!(ShaKind::Sha256.bank(), 0x3FF0_3090);
        assert_eq!(SHA_TEXT, 0x3FF0_3000);
    }

    #[test]
    fn digest_lengths_are_the_standard_ones() {
        assert_eq!(ShaKind::Sha1.digest_len(), 20);
        assert_eq!(ShaKind::Sha256.digest_len(), 32);
    }

    #[test]
    fn an_empty_message_pads_to_one_block() {
        let mut out = [0xAAu8; 128];
        let blocks = pad_tail(&[], 0, &mut out);
        assert_eq!(blocks, 1);
        // 0x80 then all zeros; length field is zero.
        assert_eq!(out[0], 0x80);
        assert!(out[1..64].iter().all(|&b| b == 0));
    }

    #[test]
    fn a_short_message_carries_its_bit_length_big_endian() {
        // "abc" -> 3 bytes -> 24 bits, in the last 8 bytes of one block.
        let mut out = [0u8; 128];
        let blocks = pad_tail(b"abc", 3, &mut out);
        assert_eq!(blocks, 1);
        assert_eq!(&out[..3], b"abc");
        assert_eq!(out[3], 0x80);
        assert_eq!(out[56..64], 24u64.to_be_bytes());
    }

    #[test]
    fn a_55_byte_remainder_still_fits_one_block() {
        // 55 bytes leaves exactly room for 0x80 + 8-byte length.
        let rem = [0x11u8; 55];
        let mut out = [0u8; 128];
        assert_eq!(pad_tail(&rem, 55, &mut out), 1);
        assert_eq!(out[55], 0x80);
        assert_eq!(out[56..64], (55u64 * 8).to_be_bytes());
    }

    #[test]
    fn a_56_byte_remainder_spills_to_two_blocks() {
        // 56 bytes: the 0x80 lands but the length field cannot, so a whole
        // second block is required. This is the case that overflows a
        // one-block buffer if a driver assumes the tail is always 64 bytes.
        let rem = [0x22u8; 56];
        let mut out = [0u8; 128];
        assert_eq!(pad_tail(&rem, 56, &mut out), 2);
        assert_eq!(out[56], 0x80);
        // Length lives in the last 8 bytes of the *second* block.
        assert_eq!(out[120..128], (56u64 * 8).to_be_bytes());
        assert!(out[57..120].iter().all(|&b| b == 0));
    }

    #[test]
    fn a_63_byte_remainder_spills_to_two_blocks() {
        let rem = [0x33u8; 63];
        let mut out = [0u8; 128];
        assert_eq!(pad_tail(&rem, 63, &mut out), 2);
        assert_eq!(out[63], 0x80);
        assert_eq!(out[120..128], (63u64 * 8).to_be_bytes());
    }
}
