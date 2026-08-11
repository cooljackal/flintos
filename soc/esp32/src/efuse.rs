// SPDX-License-Identifier: Apache-2.0

//! eFuse block 0, and the factory MAC address in it.
//!
//! One fact, and it is needed in two places already: the radio binds its RF
//! calibration to the part's MAC so a calibration restored onto different
//! silicon is refused, and anything that ever brings up a network interface
//! needs the same six bytes.
//!
//! # The layout is bit offsets, not bytes
//!
//! esp-idf describes the factory MAC as six 8-bit fields at *bit* offsets
//! within block 0, and in descending order:
//!
//! ```text
//! MAC_FACTORY[0] = BLK0 bits 72..79
//! MAC_FACTORY[1] = BLK0 bits 64..71
//! MAC_FACTORY[2] = BLK0 bits 56..63
//! MAC_FACTORY[3] = BLK0 bits 48..55
//! MAC_FACTORY[4] = BLK0 bits 40..47
//! MAC_FACTORY[5] = BLK0 bits 32..39
//! ```
//!
//! Bit 32 is the first bit of word 1, so the whole address lives in
//! `RDATA1` and the low half of `RDATA2` — and it reads out **backwards**
//! relative to the byte order everything prints it in. Extracting it in the
//! obvious little-endian way gives a plausible-looking MAC with its bytes
//! reversed, which is exactly the sort of wrong answer that survives review.
//!
//! Verified against hardware rather than argued: `espflash board-info`
//! reports the same address this returns.

use crate::addr::EFUSE_BASE;

/// `EFUSE_BLK0_RDATA1_REG` — block 0, word 1.
const BLK0_RDATA1: u32 = EFUSE_BASE + 0x04;
/// `EFUSE_BLK0_RDATA2_REG` — block 0, word 2.
const BLK0_RDATA2: u32 = EFUSE_BASE + 0x08;

/// The factory MAC address, as it is printed.
///
/// # Safety
/// Reads two eFuse registers. No side effects; eFuse is read-only here.
pub unsafe fn base_mac() -> [u8; 6] {
    let w1 = unsafe { (BLK0_RDATA1 as *const u32).read_volatile() };
    let w2 = unsafe { (BLK0_RDATA2 as *const u32).read_volatile() };
    mac_from_words(w1, w2)
}

/// The bit extraction, split out so it can be tested against a known part
/// without an eFuse to read.
///
/// See the module docs for why the order looks inverted: the fields descend
/// through the block, so byte 0 of the address is the *highest* of the six.
pub const fn mac_from_words(w1: u32, w2: u32) -> [u8; 6] {
    [
        ((w2 >> 8) & 0xFF) as u8,  // bits 72..79
        (w2 & 0xFF) as u8,         // bits 64..71
        ((w1 >> 24) & 0xFF) as u8, // bits 56..63
        ((w1 >> 16) & 0xFF) as u8, // bits 48..55
        ((w1 >> 8) & 0xFF) as u8,  // bits 40..47
        (w1 & 0xFF) as u8,         // bits 32..39
    ]
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registers_are_where_esp_idf_reads_them() {
        assert_eq!(EFUSE_BASE, 0x3FF5_A000);
        assert_eq!(BLK0_RDATA1, 0x3FF5_A004);
        assert_eq!(BLK0_RDATA2, 0x3FF5_A008);
    }

    #[test]
    fn a_real_part_decodes_to_the_address_espflash_reports() {
        // Read off the ESP32-WROOM this was brought up on. `espflash
        // board-info` says c0:49:ef:d1:13:cc, and these are the two words
        // that produced it -- so this pins the bit order against hardware
        // rather than against a reading of the header.
        let w1 = 0xEF_D1_13_CC_u32;
        let w2 = 0x0000_C049;
        assert_eq!(
            mac_from_words(w1, w2),
            [0xC0, 0x49, 0xEF, 0xD1, 0x13, 0xCC]
        );
    }

    #[test]
    fn the_byte_order_is_not_the_obvious_one() {
        // The failure this guards: extracting little-endian from the two
        // words gives a well-formed MAC with its bytes reversed, which looks
        // entirely plausible in a log.
        let got = mac_from_words(0xEF_D1_13_CC, 0x0000_C049);
        let naive = [0xCC, 0x13, 0xD1, 0xEF, 0x49, 0xC0];
        assert_ne!(got, naive, "reversed is the wrong answer, not a variant");
    }

    #[test]
    fn the_upper_half_of_the_second_word_is_ignored() {
        // Only bits 64..79 of block 0 belong to the MAC. The rest of RDATA2
        // holds other fuses, and letting them through would corrupt the
        // address on any part that has them set.
        let a = mac_from_words(0x1122_3344, 0x0000_5566);
        let b = mac_from_words(0x1122_3344, 0xFFFF_5566);
        assert_eq!(a, b, "bits above 79 must not reach the address");
    }
}
