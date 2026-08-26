// SPDX-License-Identifier: Apache-2.0

//! Board-selected persistent region; the kernel adapter only needs NorFlash.

#[cfg(feature = "esp32-drivers")]
pub type NvsFlash = esp32_flash::FlashRegion;
#[cfg(feature = "rp2040-drivers")]
pub type NvsFlash = rp2040_flash::FlashRegion;

/// Open the partition reserved by the selected board's image layout.
///
/// # Safety
/// No other owner may access the partition. Obey the selected driver's
/// cache/XIP-off requirements; in particular do not attach a flash-reading
/// debugger during programming. RP2040 additionally enforces exclusive opens.
pub unsafe fn nvs_flash() -> Option<NvsFlash> {
    let (offset, len) = crate::active::NVS_PARTITION;
    #[cfg(feature = "esp32-drivers")]
    {
        Some(NvsFlash::new(offset, len))
    }
    #[cfg(feature = "rp2040-drivers")]
    {
        #[cfg(target_arch = "arm")]
        {
            unsafe extern "C" {
                static _nvs_partition_start: u8;
                static _nvs_partition_end: u8;
            }
            if core::ptr::addr_of!(_nvs_partition_start) as u32 != soc_rp2040::XIP_BASE + offset
                || core::ptr::addr_of!(_nvs_partition_end) as u32
                    != soc_rp2040::XIP_BASE + offset + len
            {
                return None;
            }
        }
        NvsFlash::open(offset, len, crate::active::FLASH_BYTES).ok()
    }
}

#[cfg(all(test, feature = "rp2040-drivers"))]
mod tests {
    #[test]
    fn reserved_partition_is_four_final_sectors_of_fitted_flash() {
        let (offset, len) = crate::active::NVS_PARTITION;
        assert_eq!(offset, 0x1fc000);
        assert_eq!(len, 4 * rp2040_flash::SECTOR_SIZE);
        assert_eq!(offset + len, crate::active::FLASH_BYTES);
        assert_eq!(crate::active::FLASH_BYTES, 2 * 1024 * 1024);
    }
}
