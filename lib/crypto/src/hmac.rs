// SPDX-License-Identifier: Apache-2.0

//! HMAC, from RFC 2104, over the hashes in this crate.
//!
//! Every authenticated step of the handshake is an HMAC: the key confirmation
//! in the 4-way exchange, the key derivation (PBKDF2 and the PRF are both HMAC
//! in a loop), and the message integrity check. It is written once, generic
//! over a [`Hash`], so HMAC-SHA1 and HMAC-SHA256 are the same code with a
//! different digest — there is no second place for the block-size or the
//! ipad/opad constants to drift.
//!
//! # The key is secret; the shape must not depend on it
//!
//! HMAC's only data-dependent step is hashing the key when it is longer than a
//! block, and that path runs the hash, which is itself data-independent. The
//! pad construction and the two hash passes are fixed regardless of key or
//! message content, so there is no secret-dependent branch here.

/// A hash this crate can drive as an HMAC core.
///
/// The three things HMAC needs of a hash: its block size (for the pads), its
/// digest size, and the ability to hash a stream. Implemented for
/// [`Sha1`](crate::Sha1) and [`Sha256`](crate::Sha256) below.
pub trait Hash: Clone {
    /// The compression block size in bytes — 64 for both SHA-1 and SHA-256.
    const BLOCK: usize;
    /// The digest size in bytes.
    const OUTPUT: usize;

    /// A fresh hasher.
    fn new() -> Self;
    /// Absorb data.
    fn update(&mut self, data: &[u8]);
    /// Finish into `out`, which must be [`OUTPUT`](Hash::OUTPUT) long.
    fn finish_into(self, out: &mut [u8]);
}

/// The largest block any hash here uses, so the pad buffers are fixed-size.
const MAX_BLOCK: usize = 64;
/// The largest digest any hash here produces.
const MAX_OUTPUT: usize = 32;

/// Compute HMAC-`H` of `data` under `key` into `out`.
///
/// `out` is filled to `H::OUTPUT` bytes; a shorter `out` takes a truncated
/// tag, which is what CMAC and some KDF steps want and is done by the caller
/// slicing, not here.
pub fn hmac<H: Hash>(key: &[u8], data: &[u8], out: &mut [u8]) {
    assert!(H::BLOCK <= MAX_BLOCK);
    assert!(H::OUTPUT <= MAX_OUTPUT);
    assert!(out.len() >= H::OUTPUT);

    // A key longer than a block is replaced by its hash; a shorter one is
    // zero-padded. Both land in a block-sized buffer.
    let mut block_key = [0u8; MAX_BLOCK];
    if key.len() > H::BLOCK {
        let mut h = H::new();
        h.update(key);
        h.finish_into(&mut block_key[..H::OUTPUT]);
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; MAX_BLOCK];
    let mut opad = [0x5cu8; MAX_BLOCK];
    for i in 0..H::BLOCK {
        ipad[i] ^= block_key[i];
        opad[i] ^= block_key[i];
    }

    // inner = H(ipad || data)
    let mut inner = [0u8; MAX_OUTPUT];
    let mut h = H::new();
    h.update(&ipad[..H::BLOCK]);
    h.update(data);
    h.finish_into(&mut inner[..H::OUTPUT]);

    // out = H(opad || inner)
    let mut h = H::new();
    h.update(&opad[..H::BLOCK]);
    h.update(&inner[..H::OUTPUT]);
    h.finish_into(&mut out[..H::OUTPUT]);
}

impl Hash for crate::Sha1 {
    const BLOCK: usize = 64;
    const OUTPUT: usize = crate::sha1::DIGEST_LEN;
    fn new() -> Self {
        crate::Sha1::new()
    }
    fn update(&mut self, data: &[u8]) {
        crate::Sha1::update(self, data)
    }
    fn finish_into(self, out: &mut [u8]) {
        out[..Self::OUTPUT].copy_from_slice(&self.finish());
    }
}

impl Hash for crate::Sha256 {
    const BLOCK: usize = 64;
    const OUTPUT: usize = crate::sha256::DIGEST_LEN;
    fn new() -> Self {
        crate::Sha256::new()
    }
    fn update(&mut self, data: &[u8]) {
        crate::Sha256::update(self, data)
    }
    fn finish_into(self, out: &mut [u8]) {
        out[..Self::OUTPUT].copy_from_slice(&self.finish());
    }
}

/// HMAC-SHA1 of `data` under `key`. Twenty-byte tag.
pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    let mut out = [0u8; 20];
    hmac::<crate::Sha1>(key, data, &mut out);
    out
}

/// HMAC-SHA256 of `data` under `key`. Thirty-two-byte tag.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    hmac::<crate::Sha256>(key, data, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hx(s: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate().take(s.len() / 2) {
            let hi = (s[i * 2] as char).to_digit(16).unwrap() as u8;
            let lo = (s[i * 2 + 1] as char).to_digit(16).unwrap() as u8;
            *o = (hi << 4) | lo;
        }
        out
    }

    // RFC 2202 (HMAC-SHA1) and RFC 4231 (HMAC-SHA256) test vectors.

    #[test]
    fn rfc2202_sha1_case1() {
        // key = 20 x 0x0b, data = "Hi There"
        let key = [0x0bu8; 20];
        assert_eq!(
            hmac_sha1(&key, b"Hi There"),
            hx(b"b617318655057264e28bc0b6fb378c8ef146be00")[..20]
        );
    }

    #[test]
    fn rfc2202_sha1_case2_short_key() {
        assert_eq!(
            hmac_sha1(b"Jefe", b"what do ya want for nothing?"),
            hx(b"effcdf6ae5eb2fa2d27416d5f184df9c259a7c79")[..20]
        );
    }

    #[test]
    fn rfc4231_sha256_case1() {
        let key = [0x0bu8; 20];
        assert_eq!(
            hmac_sha256(&key, b"Hi There"),
            hx(b"b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7")
        );
    }

    #[test]
    fn rfc4231_sha256_case2_short_key() {
        assert_eq!(
            hmac_sha256(b"Jefe", b"what do ya want for nothing?"),
            hx(b"5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
        );
    }

    #[test]
    fn a_key_longer_than_a_block_is_hashed_first() {
        // RFC 4231 case 4: 131-byte key exercises the long-key branch.
        let key = [0xaau8; 131];
        assert_eq!(
            hmac_sha256(&key, b"Test Using Larger Than Block-Size Key - Hash Key First"),
            hx(b"60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54")
        );
    }
}
