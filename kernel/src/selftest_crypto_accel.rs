// SPDX-License-Identifier: Apache-2.0

//! SHA and AES accelerator self-tests. Included by [`crate::selftest`].
//!
//! Pure compute: no pins, no GPIO, no board wiring — the cleanest self-test in
//! the suite. Each check runs the *hardware* accelerator over a known vector
//! and then, on the same input, runs the *software* [`crypto`] primitive the
//! hardware is meant to stand in for, and asserts the two agree byte for byte.
//!
//! That double assertion is the whole point of #33. A single hardware digest
//! that matches a hard-coded vector proves the vector was typed correctly. The
//! cross-check proves something stronger: that the hardware and the software —
//! which will be swapped for each other behind the WPA crypto table — produce
//! the identical bytes, so a hardware bug (a byte-swap, a wrong clock gate)
//! cannot be mistaken for a protocol bug later. The classic ESP32 trap is
//! digest endianness, and a byte-swapped digest fails here loudly.

use super::Check;

/// HW SHA-1 matches both the FIPS-180 vector and the software `crypto::Sha1`.
#[cfg(target_os = "none")]
pub(crate) fn hw_sha1_matches_software() -> Check {
    use crypto::Sha1;

    // FIPS 180-4 example: SHA-1("abc").
    const ABC: &[u8] = b"abc";
    const ABC_SHA1: [u8; 20] = [
        0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50, 0xc2,
        0x6c, 0x9c, 0xd0, 0xd8, 0x9d,
    ];

    for (msg, label) in [(ABC, "abc"), (b"".as_slice(), "empty")] {
        let hw = unsafe { esp32_crypto::sha1(msg) }.map_err(|_| "SHA-1 accelerator timed out")?;
        let sw = Sha1::digest(msg);
        if hw != sw {
            return Err("HW SHA-1 disagrees with software crypto::Sha1 (endianness?)");
        }
        if msg == ABC && hw != ABC_SHA1 {
            return Err("HW SHA-1(\"abc\") does not match the FIPS-180 vector");
        }
        let _ = label;
    }
    Ok(())
}

/// HW SHA-256 matches both the FIPS-180 vector and `crypto::Sha256`.
#[cfg(target_os = "none")]
pub(crate) fn hw_sha256_matches_software() -> Check {
    use crypto::Sha256;

    // FIPS 180-4 example: SHA-256("abc").
    const ABC: &[u8] = b"abc";
    const ABC_SHA256: [u8; 32] = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22,
        0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00,
        0x15, 0xad,
    ];

    // A multi-block message too, so a broken CONTINUE step (state not folded
    // across blocks) is caught, not just single-block START.
    let long = [0x61u8; 100]; // 100 'a' bytes: two blocks after padding.

    for (msg, check_vector) in [(ABC, true), (long.as_slice(), false)] {
        let hw = unsafe { esp32_crypto::sha256(msg) }.map_err(|_| "SHA-256 accelerator timed out")?;
        let sw = Sha256::digest(msg);
        if hw != sw {
            return Err("HW SHA-256 disagrees with software crypto::Sha256 (endianness?)");
        }
        if check_vector && hw != ABC_SHA256 {
            return Err("HW SHA-256(\"abc\") does not match the FIPS-180 vector");
        }
    }
    Ok(())
}

/// HW AES-128 ECB matches the FIPS-197 vector, the software `crypto::Aes128`,
/// and round-trips through decrypt.
#[cfg(target_os = "none")]
pub(crate) fn hw_aes128_matches_software() -> Check {
    use crypto::Aes128;
    use esp32_crypto::{Aes, AesKey};

    // FIPS-197 Appendix C.1.
    const KEY: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const PT: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    const CT: [u8; 16] = [
        0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4, 0xc5,
        0x5a,
    ];

    let aes = Aes::new(AesKey::Bits128(KEY));

    let mut blk = PT;
    unsafe { aes.encrypt_block(&mut blk) }.map_err(|_| "AES-128 encrypt timed out")?;
    if blk != CT {
        return Err("HW AES-128 encrypt does not match the FIPS-197 vector");
    }
    let sw = Aes128::new(&KEY);
    let mut sw_blk = PT;
    sw.encrypt_block(&mut sw_blk);
    if blk != sw_blk {
        return Err("HW AES-128 disagrees with software crypto::Aes128");
    }

    unsafe { aes.decrypt_block(&mut blk) }.map_err(|_| "AES-128 decrypt timed out")?;
    if blk != PT {
        return Err("HW AES-128 decrypt did not round-trip");
    }
    Ok(())
}

/// HW AES-256 ECB matches the FIPS-197 vector and `crypto::Aes256`.
#[cfg(target_os = "none")]
pub(crate) fn hw_aes256_matches_software() -> Check {
    use crypto::Aes256;
    use esp32_crypto::{Aes, AesKey};

    // FIPS-197 Appendix C.3.
    const KEY: [u8; 32] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];
    const PT: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    const CT: [u8; 16] = [
        0x8e, 0xa2, 0xb7, 0xca, 0x51, 0x67, 0x45, 0xbf, 0xea, 0xfc, 0x49, 0x90, 0x4b, 0x49, 0x60,
        0x89,
    ];

    let aes = Aes::new(AesKey::Bits256(KEY));

    let mut blk = PT;
    unsafe { aes.encrypt_block(&mut blk) }.map_err(|_| "AES-256 encrypt timed out")?;
    if blk != CT {
        return Err("HW AES-256 encrypt does not match the FIPS-197 vector");
    }
    let sw = Aes256::new(&KEY);
    let mut sw_blk = PT;
    sw.encrypt_block(&mut sw_blk);
    if blk != sw_blk {
        return Err("HW AES-256 disagrees with software crypto::Aes256");
    }

    unsafe { aes.decrypt_block(&mut blk) }.map_err(|_| "AES-256 decrypt timed out")?;
    if blk != PT {
        return Err("HW AES-256 decrypt did not round-trip");
    }
    Ok(())
}

// Host stand-ins: there is no accelerator to drive. The cross-check against the
// software lib is the same on both sides, but only the chip has the hardware to
// disagree, so the host build just reports the shape compiles.
#[cfg(not(target_os = "none"))]
pub(crate) fn hw_sha1_matches_software() -> Check {
    Ok(())
}
#[cfg(not(target_os = "none"))]
pub(crate) fn hw_sha256_matches_software() -> Check {
    Ok(())
}
#[cfg(not(target_os = "none"))]
pub(crate) fn hw_aes128_matches_software() -> Check {
    Ok(())
}
#[cfg(not(target_os = "none"))]
pub(crate) fn hw_aes256_matches_software() -> Check {
    Ok(())
}
