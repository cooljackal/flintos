// SPDX-License-Identifier: Apache-2.0

//! The ESP32 AES accelerator: single-block AES-128/256 ECB.
//!
//! One 16-byte block in, one out. This is the raw block transform — no chaining
//! mode, no padding. ECB of a single block is exactly what a mode built on top
//! (CBC, CTR, the CCM/CMAC in the `crypto` lib) needs as its primitive, and it
//! is the cleanest thing to check against a known-answer vector.
//!
//! # Sequence (esp-idf `aes_hal.c` / `aes_ll.h`)
//!
//! Per block: write `MODE` (which encodes encrypt/decrypt and key length),
//! write the key words, write the four input words, pulse `START`, wait until
//! `IDLE` reads `1`, then read the four output words. The hardware holds no
//! state between blocks, so the key and mode are rewritten every call — which
//! is why an [`Aes`] just stores the key bytes rather than a schedule.
//!
//! # Byte order
//!
//! The **opposite** of SHA. esp-idf's block path writes key and text words
//! straight through with `DPORT_REG_WRITE` of a little-endian load and reads
//! the output back the same way — no swap anywhere (`aes_ll_write_key`,
//! `aes_ll_write_block`, `aes_ll_read_block`). So every word here is
//! `u32::from_le_bytes` / `to_le_bytes`, and the `AES_ENDIAN` register is left
//! at its reset default, exactly as the esp-idf block driver leaves it.

use soc_esp32::addr::AES_BASE;
use soc_esp32::dport::{self, CryptoClockBit};
use soc_esp32::poll;

use crate::CryptoError;

const START_REG: u32 = AES_BASE; // + 0x00
const IDLE_REG: u32 = AES_BASE + 0x04;
const MODE_REG: u32 = AES_BASE + 0x08;
const KEY_BASE: u32 = AES_BASE + 0x10;
const TEXT_BASE: u32 = AES_BASE + 0x30;

/// `ESP_AES_STATE_IDLE`: `AES_IDLE_REG` reads this when the block is done.
const STATE_IDLE: u32 = 1;

/// Mode-register bit that selects decrypt over encrypt.
const MODE_DECRYPT: u32 = 4;

/// An AES key: 128- or 256-bit. (192-bit is legal on the hardware too, but the
/// software lib the driver is checked against ships only 128 and 256, so those
/// are the two offered here.)
#[derive(Debug, Clone, Copy)]
pub enum AesKey {
    /// AES-128.
    Bits128([u8; 16]),
    /// AES-256.
    Bits256([u8; 32]),
}

impl AesKey {
    fn bytes(&self) -> &[u8] {
        match self {
            AesKey::Bits128(k) => k,
            AesKey::Bits256(k) => k,
        }
    }

    /// The `MODE_REG` value for this key length and direction.
    ///
    /// esp-idf `aes_ll_set_mode`: `(decrypt ? 4 : 0) + (key_bytes / 8 - 2)`.
    /// 128-bit → 0, 256-bit → 2; decrypt adds 4.
    fn mode(&self, decrypt: bool) -> u32 {
        let key_words = self.bytes().len() / 8;
        let base = if decrypt { MODE_DECRYPT } else { 0 };
        base + (key_words as u32 - 2)
    }
}

/// The AES accelerator, bound to one key.
///
/// Cheap to construct — it only remembers the key bytes; the hardware is
/// touched per block, not here.
pub struct Aes {
    key: AesKey,
}

impl Aes {
    /// Bind the accelerator to `key`.
    pub const fn new(key: AesKey) -> Self {
        Self { key }
    }

    /// Encrypt one 16-byte block in place (ECB).
    ///
    /// # Safety
    /// Takes the AES accelerator for the duration and gates its clock. Do not
    /// call concurrently with another AES user.
    pub unsafe fn encrypt_block(&self, block: &mut [u8; 16]) -> Result<(), CryptoError> {
        self.transform(block, false)
    }

    /// Decrypt one 16-byte block in place (ECB).
    ///
    /// # Safety
    /// As [`Aes::encrypt_block`].
    pub unsafe fn decrypt_block(&self, block: &mut [u8; 16]) -> Result<(), CryptoError> {
        self.transform(block, true)
    }

    /// The shared block path: mode, key, text in; `START`; wait idle; text out.
    unsafe fn transform(&self, block: &mut [u8; 16], decrypt: bool) -> Result<(), CryptoError> {
        dport::enable_crypto(CryptoClockBit::AES);

        dport::write(MODE_REG, self.key.mode(decrypt));

        // Key and text words go in little-endian, straight through (no swap).
        for (i, word) in self.key.bytes().chunks_exact(4).enumerate() {
            let v = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
            dport::write(KEY_BASE + (i as u32) * 4, v);
        }
        for (i, word) in block.chunks_exact(4).enumerate() {
            let v = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
            dport::write(TEXT_BASE + (i as u32) * 4, v);
        }

        dport::write(START_REG, 1);
        poll::until(|| dport::read(IDLE_REG) == STATE_IDLE, poll::DEFAULT_SPINS)
            .map_err(|_| CryptoError::Timeout)?;

        for (i, word) in block.chunks_exact_mut(4).enumerate() {
            let v = dport::read(TEXT_BASE + (i as u32) * 4);
            word.copy_from_slice(&v.to_le_bytes());
        }

        dport::disable_crypto(CryptoClockBit::AES);
        Ok(())
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_map_matches_esp_idf() {
        // hwcrypto_reg.h, relative to DR_REG_AES_BASE = 0x3FF0_1000.
        assert_eq!(START_REG, 0x3FF0_1000);
        assert_eq!(IDLE_REG, 0x3FF0_1004);
        assert_eq!(MODE_REG, 0x3FF0_1008);
        assert_eq!(KEY_BASE, 0x3FF0_1010);
        assert_eq!(TEXT_BASE, 0x3FF0_1030);
    }

    #[test]
    fn mode_encodes_length_and_direction() {
        let k128 = AesKey::Bits128([0; 16]);
        let k256 = AesKey::Bits256([0; 32]);
        // encrypt: 128 -> 0, 256 -> 2
        assert_eq!(k128.mode(false), 0);
        assert_eq!(k256.mode(false), 2);
        // decrypt adds 4
        assert_eq!(k128.mode(true), 4);
        assert_eq!(k256.mode(true), 6);
    }
}
