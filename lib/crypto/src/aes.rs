// SPDX-License-Identifier: Apache-2.0

//! AES, from FIPS 197.
//!
//! The block cipher under the second half of WPA2/WPA3: CCMP encrypts data
//! frames with AES in CCM mode, the GTK arrives key-wrapped with AES, and the
//! EAPOL-Key MIC on modern APs is AES-CMAC. All of those are modes built on a
//! single 16-byte block operation, which is what this provides — [`Aes128`]
//! and [`Aes256`], each with `encrypt_block` and `decrypt_block`. The modes
//! themselves live in their own modules over this one.
//!
//! # Side channels, stated plainly
//!
//! This is a table-driven software AES: `SubBytes` indexes a 256-byte S-box.
//! On a machine with a data cache that is a timing side channel on the key,
//! and this implementation does **not** defend against it. Two things make
//! that acceptable here and both are recorded rather than assumed: the ESP32
//! reaches the S-box in DRAM without a data cache in the desktop sense, and —
//! the real point — the production path is the **AES hardware accelerator**
//! (#33), which is constant-time. This software core is the reference the
//! accelerator is checked against and the fallback when it is absent, exactly
//! as the module docs describe for the hashes. Where a caller has a secret and
//! no accelerator, that trade-off is theirs to note.

/// FIPS 197 Figure 7: the S-box.
const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

/// The inverse S-box, for decryption.
const INV_SBOX: [u8; 256] = {
    let mut inv = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        inv[SBOX[i] as usize] = i as u8;
        i += 1;
    }
    inv
};

/// Round constants, FIPS 197 §5.2. Only the first ten are ever needed (AES-256
/// uses seven).
const RCON: [u8; 11] = [
    0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36,
];

/// The most round keys any variant here needs: AES-256 has 14 rounds, so 15
/// round keys of 16 bytes.
const MAX_ROUND_KEYS: usize = 15;

/// An expanded AES key, ready to encrypt or decrypt 16-byte blocks.
///
/// Built once with [`Aes128::new`] or [`Aes256::new`]; the schedule holds the
/// secret, so it is not `Debug` and its bytes never leave this type.
#[derive(Clone)]
struct Aes {
    round_keys: [[u8; 16]; MAX_ROUND_KEYS],
    rounds: usize,
}

impl Aes {
    /// Expand `key` (16 or 32 bytes) into the round-key schedule.
    fn expand(key: &[u8], rounds: usize) -> Self {
        let nk = key.len() / 4; // key words: 4 (AES-128) or 8 (AES-256)
        let total_words = 4 * (rounds + 1);
        let mut w = [[0u8; 4]; 4 * (MAX_ROUND_KEYS)];

        for i in 0..nk {
            w[i] = [key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]];
        }

        for i in nk..total_words {
            let mut temp = w[i - 1];
            if i % nk == 0 {
                // RotWord, SubWord, then XOR the round constant.
                temp = [temp[1], temp[2], temp[3], temp[0]];
                for b in &mut temp {
                    *b = SBOX[*b as usize];
                }
                temp[0] ^= RCON[i / nk];
            } else if nk > 6 && i % nk == 4 {
                // AES-256 only: an extra SubWord every eighth word.
                for b in &mut temp {
                    *b = SBOX[*b as usize];
                }
            }
            for j in 0..4 {
                w[i][j] = w[i - nk][j] ^ temp[j];
            }
        }

        let mut round_keys = [[0u8; 16]; MAX_ROUND_KEYS];
        for r in 0..=rounds {
            for c in 0..4 {
                round_keys[r][4 * c..4 * c + 4].copy_from_slice(&w[4 * r + c]);
            }
        }

        Self { round_keys, rounds }
    }

    fn encrypt_block(&self, block: &mut [u8; 16]) {
        add_round_key(block, &self.round_keys[0]);
        for r in 1..self.rounds {
            sub_bytes(block);
            shift_rows(block);
            mix_columns(block);
            add_round_key(block, &self.round_keys[r]);
        }
        // The final round has no MixColumns.
        sub_bytes(block);
        shift_rows(block);
        add_round_key(block, &self.round_keys[self.rounds]);
    }

    fn decrypt_block(&self, block: &mut [u8; 16]) {
        add_round_key(block, &self.round_keys[self.rounds]);
        for r in (1..self.rounds).rev() {
            inv_shift_rows(block);
            inv_sub_bytes(block);
            add_round_key(block, &self.round_keys[r]);
            inv_mix_columns(block);
        }
        inv_shift_rows(block);
        inv_sub_bytes(block);
        add_round_key(block, &self.round_keys[0]);
    }
}

/// AES-128: a 16-byte key, 10 rounds.
#[derive(Clone)]
pub struct Aes128(Aes);

/// AES-256: a 32-byte key, 14 rounds.
#[derive(Clone)]
pub struct Aes256(Aes);

impl Aes128 {
    /// Expand a 128-bit key.
    pub fn new(key: &[u8; 16]) -> Self {
        Self(Aes::expand(key, 10))
    }
    /// Encrypt one block in place.
    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        self.0.encrypt_block(block)
    }
    /// Decrypt one block in place.
    pub fn decrypt_block(&self, block: &mut [u8; 16]) {
        self.0.decrypt_block(block)
    }
}

impl Aes256 {
    /// Expand a 256-bit key.
    pub fn new(key: &[u8; 32]) -> Self {
        Self(Aes::expand(key, 14))
    }
    /// Encrypt one block in place.
    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        self.0.encrypt_block(block)
    }
    /// Decrypt one block in place.
    pub fn decrypt_block(&self, block: &mut [u8; 16]) {
        self.0.decrypt_block(block)
    }
}

// ── The round transforms, on a column-major 16-byte state ────────────────────

fn add_round_key(state: &mut [u8; 16], key: &[u8; 16]) {
    for (s, k) in state.iter_mut().zip(key.iter()) {
        *s ^= *k;
    }
}

fn sub_bytes(state: &mut [u8; 16]) {
    for b in state.iter_mut() {
        *b = SBOX[*b as usize];
    }
}

fn inv_sub_bytes(state: &mut [u8; 16]) {
    for b in state.iter_mut() {
        *b = INV_SBOX[*b as usize];
    }
}

/// Row `r` rotates left by `r`. In column-major order the byte at row `r`,
/// column `c` is `state[r + 4c]`.
fn shift_rows(state: &mut [u8; 16]) {
    let old = *state;
    for r in 0..4 {
        for c in 0..4 {
            state[r + 4 * c] = old[r + 4 * ((c + r) % 4)];
        }
    }
}

fn inv_shift_rows(state: &mut [u8; 16]) {
    let old = *state;
    for r in 0..4 {
        for c in 0..4 {
            state[r + 4 * c] = old[r + 4 * ((c + 4 - r) % 4)];
        }
    }
}

/// Multiply by x in GF(2^8) with the AES reduction polynomial 0x11b.
fn xtime(a: u8) -> u8 {
    let hi = a & 0x80;
    let shifted = a << 1;
    if hi != 0 {
        shifted ^ 0x1b
    } else {
        shifted
    }
}

/// General GF(2^8) multiply, for the inverse mix columns coefficients.
fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

fn mix_columns(state: &mut [u8; 16]) {
    for c in 0..4 {
        let a0 = state[4 * c];
        let a1 = state[4 * c + 1];
        let a2 = state[4 * c + 2];
        let a3 = state[4 * c + 3];
        state[4 * c] = xtime(a0) ^ (xtime(a1) ^ a1) ^ a2 ^ a3;
        state[4 * c + 1] = a0 ^ xtime(a1) ^ (xtime(a2) ^ a2) ^ a3;
        state[4 * c + 2] = a0 ^ a1 ^ xtime(a2) ^ (xtime(a3) ^ a3);
        state[4 * c + 3] = (xtime(a0) ^ a0) ^ a1 ^ a2 ^ xtime(a3);
    }
}

fn inv_mix_columns(state: &mut [u8; 16]) {
    for c in 0..4 {
        let a0 = state[4 * c];
        let a1 = state[4 * c + 1];
        let a2 = state[4 * c + 2];
        let a3 = state[4 * c + 3];
        state[4 * c] = gmul(a0, 14) ^ gmul(a1, 11) ^ gmul(a2, 13) ^ gmul(a3, 9);
        state[4 * c + 1] = gmul(a0, 9) ^ gmul(a1, 14) ^ gmul(a2, 11) ^ gmul(a3, 13);
        state[4 * c + 2] = gmul(a0, 13) ^ gmul(a1, 9) ^ gmul(a2, 14) ^ gmul(a3, 11);
        state[4 * c + 3] = gmul(a0, 11) ^ gmul(a1, 13) ^ gmul(a2, 9) ^ gmul(a3, 14);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hx16(s: &[u8]) -> [u8; 16] {
        let mut out = [0u8; 16];
        for (i, o) in out.iter_mut().enumerate() {
            let hi = (s[i * 2] as char).to_digit(16).unwrap() as u8;
            let lo = (s[i * 2 + 1] as char).to_digit(16).unwrap() as u8;
            *o = (hi << 4) | lo;
        }
        out
    }

    // FIPS 197 Appendix C: the worked examples.

    #[test]
    fn fips197_aes128() {
        let key = hx16(b"000102030405060708090a0b0c0d0e0f");
        let cipher = Aes128::new(&key);
        let mut block = hx16(b"00112233445566778899aabbccddeeff");
        cipher.encrypt_block(&mut block);
        assert_eq!(block, hx16(b"69c4e0d86a7b0430d8cdb78070b4c55a"));
        cipher.decrypt_block(&mut block);
        assert_eq!(block, hx16(b"00112233445566778899aabbccddeeff"));
    }

    #[test]
    fn fips197_aes256() {
        let mut key = [0u8; 32];
        for (i, b) in key.iter_mut().enumerate() {
            *b = i as u8;
        }
        let cipher = Aes256::new(&key);
        let mut block = hx16(b"00112233445566778899aabbccddeeff");
        cipher.encrypt_block(&mut block);
        assert_eq!(block, hx16(b"8ea2b7ca516745bfeafc49904b496089"));
        cipher.decrypt_block(&mut block);
        assert_eq!(block, hx16(b"00112233445566778899aabbccddeeff"));
    }

    #[test]
    fn fips197_aes128_key_schedule_example() {
        // FIPS 197 Appendix A.1 all-zero variant is well-known: encrypting the
        // zero block under the zero key.
        let cipher = Aes128::new(&[0u8; 16]);
        let mut block = [0u8; 16];
        cipher.encrypt_block(&mut block);
        assert_eq!(block, hx16(b"66e94bd4ef8a2c3b884cfa59ca342b2e"));
    }

    #[test]
    fn encrypt_decrypt_round_trips_for_arbitrary_data() {
        let key = hx16(b"2b7e151628aed2a6abf7158809cf4f3c");
        let cipher = Aes128::new(&key);
        for seed in 0..64u8 {
            let mut block: [u8; 16] = core::array::from_fn(|i| seed.wrapping_add(i as u8));
            let original = block;
            cipher.encrypt_block(&mut block);
            assert_ne!(block, original, "ciphertext equals plaintext, seed {seed}");
            cipher.decrypt_block(&mut block);
            assert_eq!(block, original, "round trip failed, seed {seed}");
        }
    }
}
