// SPDX-License-Identifier: Apache-2.0

//! The ESP32's hardware random number generator.
//!
//! One register. Each read returns a fresh 32-bit value.
//!
//! # How random it actually is
//!
//! This matters more than the API does, because reading the register and
//! assuming the result is unpredictable is a security bug that looks exactly
//! like working code.
//!
//! The RNG harvests entropy from the RF subsystem's thermal noise and, failing
//! that, from an internal fast oscillator sampled against the slower RTC clock.
//! Espressif's own guidance is that the output is **only** suitable for
//! cryptographic use when the WiFi or Bluetooth radio is running. FlintOS does not
//! bring up the radio (see #36), so on this kernel today:
//!
//! - Fine for a random backoff, a jittered retry, a test seed, a nonce whose
//!   only requirement is "usually different".
//! - **Not** fine for a key, a token, or anything an attacker benefits from
//!   guessing.
//!
//! The distinction is deliberately not hidden behind a comforting name.
//! [`read_u32`] says what it does; nothing here is called `secure_random`.
//!
//! Register from the ESP32 TRM chapter 24 and esp-idf's `esp_random()`, which
//! reads the same address.

#![no_std]

/// `WDEV_RND_REG`. Reads return a new value each time; there is nothing to
/// configure and nothing to enable.
pub const RNG_DATA_REG: u32 = 0x3FF7_5144;

/// One 32-bit value from the hardware generator.
///
/// # Safety
/// Reads a hardware register. Safe in the sense that it cannot corrupt
/// anything, but see the module docs before trusting the value with a secret.
#[inline]
pub unsafe fn read_u32() -> u32 {
    (RNG_DATA_REG as *const u32).read_volatile()
}

/// Fill `buf` with random bytes.
///
/// # Safety
/// Reads a hardware register. See the module docs on entropy quality.
pub unsafe fn fill(buf: &mut [u8]) {
    fill_from(|| read_u32(), buf)
}

/// Fill `buf` from an arbitrary 32-bit source.
///
/// Split out from [`fill`] so the chunking can be tested on a host. The
/// register read cannot be, but the part that actually goes wrong — the tail
/// shorter than a word — can, and does in most hand-written versions of this.
pub fn fill_from(mut next: impl FnMut() -> u32, buf: &mut [u8]) {
    let mut chunks = buf.chunks_exact_mut(4);
    for chunk in &mut chunks {
        chunk.copy_from_slice(&next().to_le_bytes());
    }
    // The remainder: 1..=3 bytes, and only consume a word if there is one.
    // Calling `next()` unconditionally would waste a read on an exactly-aligned
    // buffer, which matters when the caller is draining a slow entropy source.
    let tail = chunks.into_remainder();
    if !tail.is_empty() {
        let word = next().to_le_bytes();
        tail.copy_from_slice(&word[..tail.len()]);
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A counting source: predictable, so the byte layout is checkable.
    fn counter() -> impl FnMut() -> u32 {
        let mut n = 0u32;
        move || {
            n += 1;
            n
        }
    }

    #[test]
    fn an_exact_multiple_of_four_uses_one_word_per_chunk() {
        let mut buf = [0u8; 8];
        fill_from(counter(), &mut buf);
        assert_eq!(buf[..4], 1u32.to_le_bytes());
        assert_eq!(buf[4..], 2u32.to_le_bytes());
    }

    #[test]
    fn a_short_tail_is_filled_and_not_overrun() {
        // Five bytes: one whole word plus a single byte. Getting this wrong
        // either panics on the copy or leaves the last bytes zero, and a
        // silently zeroed tail is the worse of the two.
        let mut buf = [0u8; 5];
        fill_from(counter(), &mut buf);
        assert_eq!(buf[..4], 1u32.to_le_bytes());
        assert_eq!(buf[4], 2u32.to_le_bytes()[0]);
    }

    #[test]
    fn every_tail_length_is_handled() {
        for len in 0..=16usize {
            let mut buf = [0u8; 16];
            fill_from(counter(), &mut buf[..len]);
            // Nothing past the requested length may be touched.
            assert!(buf[len..].iter().all(|&b| b == 0), "wrote past len={len}");
        }
    }

    #[test]
    fn an_empty_buffer_consumes_no_entropy() {
        let mut calls = 0u32;
        let mut buf: [u8; 0] = [];
        fill_from(
            || {
                calls += 1;
                0
            },
            &mut buf,
        );
        assert_eq!(calls, 0, "an empty fill must not read the generator");
    }

    #[test]
    fn an_aligned_buffer_wastes_no_read() {
        // Four bytes is exactly one word. A version that always reads a word
        // for the tail would take two, which matters when the source is slow.
        let mut calls = 0u32;
        let mut buf = [0u8; 4];
        fill_from(
            || {
                calls += 1;
                0xAABB_CCDD
            },
            &mut buf,
        );
        assert_eq!(calls, 1);
    }

    #[test]
    fn the_register_address_matches_esp_idf() {
        // esp_random() reads this same address; a wrong one here returns
        // whatever unrelated peripheral lives there, which may well look
        // random enough to pass a casual glance.
        assert_eq!(RNG_DATA_REG, 0x3FF7_5144);
    }
}
