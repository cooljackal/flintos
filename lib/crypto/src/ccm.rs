// SPDX-License-Identifier: Apache-2.0

//! AES-CCM, from RFC 3610 / NIST SP 800-38C.
//!
//! The cipher on the wire: CCMP protects every WPA2 data frame with AES-128 in
//! CCM mode — counter mode for confidentiality, CBC-MAC for integrity, both
//! from the one AES block operation. `ccmp_encrypt`/`ccmp_decrypt` in the
//! crypto table are this with the 802.11 nonce and additional-data framing on
//! top; that framing is protocol glue and lives with the supplicant, while the
//! transform itself is here and generic.
//!
//! # One primitive, two jobs, and why the order matters on decrypt
//!
//! Encryption computes the MIC over the plaintext first, then counter-encrypts
//! plaintext and MIC together. Decryption must therefore counter-decrypt
//! first, then recompute the MIC over the recovered plaintext and compare — and
//! it must **not reveal the plaintext if the MIC is wrong**, because releasing
//! unauthenticated plaintext is the classic CCM misuse. [`decrypt`] returns an
//! error and zeroes its output on a MIC mismatch.
//!
//! Parameters are fixed to what CCMP uses and what the RFC 3610 vectors
//! exercise: a 13-byte nonce (so the length field L is 2) and a caller-chosen
//! MIC length M (8 for CCMP).

use crate::aes::Aes128;

/// CCM with a 13-byte nonce fixes L, the message-length field, at 2 bytes.
const L: usize = 2;
const NONCE_LEN: usize = 15 - L;

/// Errors from [`decrypt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The MIC did not verify: wrong key, wrong nonce, or altered data. The
    /// output buffer is zeroed.
    Mic,
    /// A size was wrong — nonce not 13 bytes, output buffer too small, or the
    /// ciphertext shorter than the MIC it should carry.
    BadLength,
}

/// The CBC-MAC that produces the CCM tag (RFC 3610 §2.2).
///
/// Returns the full 16-byte CBC-MAC result; the caller keeps the first M
/// bytes. Runs over B0 (the formatted nonce and length), the length-prefixed
/// and padded AAD, and the zero-padded message.
fn cbc_mac(cipher: &Aes128, nonce: &[u8], aad: &[u8], msg: &[u8], mic_len: usize) -> [u8; 16] {
    let mut x = [0u8; 16];

    // B0: flags ‖ nonce ‖ l(m). flags = 64·Adata + 8·(M-2)/2 + (L-1).
    let adata = if aad.is_empty() { 0 } else { 1 };
    let mut b0 = [0u8; 16];
    b0[0] = 64 * adata + 8 * (((mic_len - 2) / 2) as u8) + (L as u8 - 1);
    b0[1..1 + NONCE_LEN].copy_from_slice(nonce);
    b0[14..16].copy_from_slice(&(msg.len() as u16).to_be_bytes());
    xor_block(&mut x, &b0);
    cipher.encrypt_block(&mut x);

    // AAD, if any: a 2-byte big-endian length prefix (for lengths that fit),
    // then the AAD, then zero padding to a block boundary. Fed block by block.
    if !aad.is_empty() {
        let mut prefixed = [0u8; 2];
        prefixed.copy_from_slice(&(aad.len() as u16).to_be_bytes());
        // Walk a virtual stream: the 2 prefix bytes then the AAD, padded.
        let mut stream_pos = 0;
        let total = 2 + aad.len();
        while stream_pos < total {
            let mut block = [0u8; 16];
            for (k, slot) in block.iter_mut().enumerate() {
                let p = stream_pos + k;
                if p < 2 {
                    *slot = prefixed[p];
                } else if p < total {
                    *slot = aad[p - 2];
                }
            }
            xor_block(&mut x, &block);
            cipher.encrypt_block(&mut x);
            stream_pos += 16;
        }
    }

    // The message, zero-padded to whole blocks.
    let mut pos = 0;
    while pos < msg.len() {
        let mut block = [0u8; 16];
        let take = (msg.len() - pos).min(16);
        block[..take].copy_from_slice(&msg[pos..pos + take]);
        xor_block(&mut x, &block);
        cipher.encrypt_block(&mut x);
        pos += 16;
    }

    x
}

/// Build counter block A_i (RFC 3610 §2.3): flags(L-1) ‖ nonce ‖ i.
fn ctr_block(nonce: &[u8], i: u16) -> [u8; 16] {
    let mut a = [0u8; 16];
    a[0] = L as u8 - 1;
    a[1..1 + NONCE_LEN].copy_from_slice(nonce);
    a[14..16].copy_from_slice(&i.to_be_bytes());
    a
}

/// Counter-mode keystream XOR over `data`, starting from counter block index
/// `start` (1 for the message; block 0 is reserved to encrypt the MIC).
fn ctr_xor(cipher: &Aes128, nonce: &[u8], data: &mut [u8], start: u16) {
    let mut counter = start;
    let mut pos = 0;
    while pos < data.len() {
        let mut s = ctr_block(nonce, counter);
        cipher.encrypt_block(&mut s);
        let take = (data.len() - pos).min(16);
        for k in 0..take {
            data[pos + k] ^= s[k];
        }
        pos += take;
        counter += 1;
    }
}

/// Encrypt `plaintext` under `key`/`nonce` with `aad` authenticated.
///
/// Writes the ciphertext (same length as the plaintext) into `out`, and
/// returns the `mic_len`-byte MIC. The 802.11 caller appends the MIC to the
/// frame; keeping it separate here avoids a second length to get wrong.
pub fn encrypt(
    key: &[u8; 16],
    nonce: &[u8],
    aad: &[u8],
    plaintext: &[u8],
    mic_len: usize,
    out: &mut [u8],
) -> Result<[u8; 16], Error> {
    if nonce.len() != NONCE_LEN || out.len() < plaintext.len() {
        return Err(Error::BadLength);
    }
    let cipher = Aes128::new(key);

    // MIC over the plaintext, then encrypt it with counter block 0.
    let t = cbc_mac(&cipher, nonce, aad, plaintext, mic_len);
    let mut s0 = ctr_block(nonce, 0);
    cipher.encrypt_block(&mut s0);
    let mut mic = [0u8; 16];
    for k in 0..mic_len {
        mic[k] = t[k] ^ s0[k];
    }

    // Encrypt the payload with counter blocks from 1.
    out[..plaintext.len()].copy_from_slice(plaintext);
    ctr_xor(&cipher, nonce, &mut out[..plaintext.len()], 1);

    Ok(mic)
}

/// Decrypt `ciphertext` and verify `mic` under `key`/`nonce`/`aad`.
///
/// Writes the recovered plaintext into `out` only if the MIC verifies; on
/// failure `out` is zeroed and [`Error::Mic`] is returned, so unauthenticated
/// plaintext never reaches the caller.
pub fn decrypt(
    key: &[u8; 16],
    nonce: &[u8],
    aad: &[u8],
    ciphertext: &[u8],
    mic: &[u8],
    out: &mut [u8],
) -> Result<(), Error> {
    if nonce.len() != NONCE_LEN || out.len() < ciphertext.len() {
        return Err(Error::BadLength);
    }
    let mic_len = mic.len();
    let cipher = Aes128::new(key);

    // Recover the plaintext with counter blocks from 1.
    out[..ciphertext.len()].copy_from_slice(ciphertext);
    ctr_xor(&cipher, nonce, &mut out[..ciphertext.len()], 1);

    // Recompute the MIC over the recovered plaintext and encrypt with block 0.
    let t = cbc_mac(&cipher, nonce, aad, &out[..ciphertext.len()], mic_len);
    let mut s0 = ctr_block(nonce, 0);
    cipher.encrypt_block(&mut s0);

    let mut diff = 0u8;
    for k in 0..mic_len {
        diff |= (t[k] ^ s0[k]) ^ mic[k];
    }
    if diff != 0 {
        for b in out[..ciphertext.len()].iter_mut() {
            *b = 0;
        }
        return Err(Error::Mic);
    }
    Ok(())
}

/// XOR a 16-byte block into `x`.
fn xor_block(x: &mut [u8; 16], b: &[u8; 16]) {
    for (xb, bb) in x.iter_mut().zip(b.iter()) {
        *xb ^= *bb;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes<const N: usize>(hexish: &[u8]) -> [u8; N] {
        // hexish is a string of hex digit pairs; decode into N bytes.
        let mut out = [0u8; N];
        for (i, o) in out.iter_mut().enumerate() {
            let hi = (hexish[i * 2] as char).to_digit(16).unwrap() as u8;
            let lo = (hexish[i * 2 + 1] as char).to_digit(16).unwrap() as u8;
            *o = (hi << 4) | lo;
        }
        out
    }

    // RFC 3610 Packet Vector #1: AES-128, 13-byte nonce, 8-byte MIC, 8-byte
    // AAD, 23-byte payload.
    #[test]
    fn rfc3610_packet_vector_1() {
        let key: [u8; 16] = bytes(b"c0c1c2c3c4c5c6c7c8c9cacbcccdcecf");
        let nonce: [u8; 13] = bytes(b"00000003020100a0a1a2a3a4a5");
        let aad: [u8; 8] = bytes(b"0001020304050607");
        let payload: [u8; 23] = bytes(b"08090a0b0c0d0e0f101112131415161718191a1b1c1d1e");

        let expected_ct: [u8; 23] =
            bytes(b"588c979a61c663d2f066d0c2c0f989806d5f6b61dac384");
        let expected_mic: [u8; 8] = bytes(b"17e8d12cfdf926e0");

        let mut ct = [0u8; 23];
        let mic = encrypt(&key, &nonce, &aad, &payload, 8, &mut ct).unwrap();
        assert_eq!(ct, expected_ct, "ciphertext");
        assert_eq!(&mic[..8], &expected_mic, "mic");

        // Round trip: decrypt verifies and recovers.
        let mut back = [0u8; 23];
        decrypt(&key, &nonce, &aad, &ct, &mic[..8], &mut back).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn a_flipped_mic_bit_fails_and_zeroes_output() {
        let key: [u8; 16] = bytes(b"c0c1c2c3c4c5c6c7c8c9cacbcccdcecf");
        let nonce: [u8; 13] = bytes(b"00000003020100a0a1a2a3a4a5");
        let aad: [u8; 8] = bytes(b"0001020304050607");
        let payload: [u8; 23] = bytes(b"08090a0b0c0d0e0f101112131415161718191a1b1c1d1e");
        let mut ct = [0u8; 23];
        let mut mic = encrypt(&key, &nonce, &aad, &payload, 8, &mut ct).unwrap();
        mic[0] ^= 0x80;

        let mut back = [0xffu8; 23];
        assert_eq!(
            decrypt(&key, &nonce, &aad, &ct, &mic[..8], &mut back),
            Err(Error::Mic)
        );
        assert_eq!(back, [0u8; 23], "unauthenticated plaintext must be withheld");
    }

    #[test]
    fn altered_aad_fails_verification() {
        let key: [u8; 16] = bytes(b"c0c1c2c3c4c5c6c7c8c9cacbcccdcecf");
        let nonce: [u8; 13] = bytes(b"00000003020100a0a1a2a3a4a5");
        let aad: [u8; 8] = bytes(b"0001020304050607");
        let payload: [u8; 23] = bytes(b"08090a0b0c0d0e0f101112131415161718191a1b1c1d1e");
        let mut ct = [0u8; 23];
        let mic = encrypt(&key, &nonce, &aad, &payload, 8, &mut ct).unwrap();

        let mut wrong_aad = aad;
        wrong_aad[0] ^= 0x01;
        let mut back = [0u8; 23];
        assert_eq!(
            decrypt(&key, &nonce, &wrong_aad, &ct, &mic[..8], &mut back),
            Err(Error::Mic)
        );
    }
}
