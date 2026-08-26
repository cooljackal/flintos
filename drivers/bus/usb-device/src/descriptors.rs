// SPDX-License-Identifier: Apache-2.0
//! CDC ACM + SDK-compatible reset interface. USB 2.1 BOS/MS OS 2.0 descriptors
//! let Windows bind its built-in serial and WinUSB drivers without an INF.
use crate::Identity;

pub const CONFIG: [u8; 84] = [
    9, 2, 84, 0, 3, 1, 0, 0x80, 125, 8, 11, 0, 2, 2, 2, 1, 0, 9, 4, 0, 0, 1, 2, 2, 1, 0, 5, 0x24,
    0, 0x10, 0x01, 5, 0x24, 1, 0, 1, 4, 0x24, 2, 2, 5, 0x24, 6, 0, 1, 7, 5, 0x81, 3, 8, 0, 16, 9,
    4, 1, 0, 2, 0x0a, 0, 0, 0, 7, 5, 0x02, 2, 64, 0, 0, 7, 5, 0x82, 2, 64, 0, 0, 9, 4, 2, 0, 0,
    0xff, 0, 1, 0,
];
pub const BOS: [u8; 33] = [
    5, 15, 33, 0, 1, 28, 16, 5, 0, 0xdf, 0x60, 0xdd, 0xd8, 0x89, 0x45, 0xc7, 0x4c, 0x9c, 0xd2,
    0x65, 0x9d, 0x9e, 0x64, 0x8a, 0x9f, 0, 0, 3, 6, 166, 0, 1, 0,
];

pub fn microsoft(out: &mut [u8; 256]) -> usize {
    out.fill(0);
    out[..10].copy_from_slice(&[10, 0, 0, 0, 0, 0, 3, 6, 166, 0]);
    out[10..18].copy_from_slice(&[8, 0, 2, 0, 2, 0, 156, 0]);
    out[18..22].copy_from_slice(&[20, 0, 3, 0]);
    out[22..28].copy_from_slice(b"WINUSB");
    out[38..46].copy_from_slice(&[128, 0, 4, 0, 1, 0, 40, 0]);
    let name = "DeviceInterfaceGUID\0";
    for (i, c) in name.encode_utf16().enumerate() {
        out[46 + 2 * i..48 + 2 * i].copy_from_slice(&c.to_le_bytes());
    }
    out[86..88].copy_from_slice(&[78, 0]);
    let guid = "{bc7398c1-73cd-4cb7-98b8-913a8fca7bf6}\0";
    for (i, c) in guid.encode_utf16().enumerate() {
        out[88 + 2 * i..90 + 2 * i].copy_from_slice(&c.to_le_bytes());
    }
    166
}

pub fn descriptor(
    identity: &Identity,
    value: u16,
    language: u16,
    out: &mut [u8; 256],
) -> Option<usize> {
    let index = value as u8;
    match (value >> 8, index) {
        (1, 0) if language == 0 => {
            out[..18].copy_from_slice(&[
                18,
                1,
                0x10,
                2,
                0xef,
                2,
                1,
                64,
                identity.vid as u8,
                (identity.vid >> 8) as u8,
                identity.pid as u8,
                (identity.pid >> 8) as u8,
                0,
                1,
                1,
                2,
                if identity.serial.is_some() { 3 } else { 0 },
                1,
            ]);
            Some(18)
        }
        (2, 0) if language == 0 => {
            out[..CONFIG.len()].copy_from_slice(&CONFIG);
            Some(CONFIG.len())
        }
        (15, 0) if language == 0 => {
            out[..BOS.len()].copy_from_slice(&BOS);
            Some(BOS.len())
        }
        (3, 0) if language == 0 => {
            out[..4].copy_from_slice(&[4, 3, 9, 4]);
            Some(4)
        }
        (3, _) if language == 0x0409 => {
            let text = match index {
                1 => identity.manufacturer,
                2 => identity.product,
                3 => identity.serial?,
                _ => return None,
            };
            let mut len = 2;
            for word in text.encode_utf16() {
                if len + 2 > 254 {
                    return None;
                }
                out[len..len + 2].copy_from_slice(&word.to_le_bytes());
                len += 2;
            }
            out[0] = len as u8;
            out[1] = 3;
            Some(len)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_walk_is_exact_and_endpoints_are_distinct() {
        let mut offset = 0;
        let mut endpoints = 0u32;
        let mut interfaces = 0;
        while offset < CONFIG.len() {
            let len = CONFIG[offset] as usize;
            assert!(len >= 2 && offset + len <= CONFIG.len());
            if CONFIG[offset + 1] == 4 {
                interfaces += 1;
            }
            if CONFIG[offset + 1] == 5 {
                let ep = CONFIG[offset + 2];
                let bit = 1 << ((ep & 15) * 2 + u8::from(ep & 128 != 0));
                assert_eq!(endpoints & bit, 0);
                endpoints |= bit;
            }
            offset += len;
        }
        assert_eq!(offset, 84);
        assert_eq!(interfaces, CONFIG[4]);
        assert_eq!(endpoints.count_ones(), 3);
    }
    #[test]
    fn microsoft_lengths_and_reset_interface_match() {
        let mut data = [0; 256];
        assert_eq!(microsoft(&mut data), 166);
        assert_eq!(u16::from_le_bytes([data[8], data[9]]), 166);
        assert_eq!(&data[14..18], &[2, 0, 156, 0]);
        assert_eq!(&data[22..30], b"WINUSB\0\0");
        assert_eq!(&data[164..166], &[0, 0]);
        assert_eq!(&BOS[29..31], &[166, 0]);
    }
}
