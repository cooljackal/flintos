// SPDX-License-Identifier: Apache-2.0

//! ESP-IDF application descriptor.
//!
//! The ESP-IDF 2nd-stage bootloader (the one espflash bundles) refuses to hand
//! off to an app whose image lacks a valid `esp_app_desc_t` at flash offset
//! 0x20 — it calls `abort()` and the chip reset-loops. This is an
//! Espressif-boot-image artifact, so it lives here in the Xtensa arch crate, not
//! in the portable kernel or the declarative board manifests. A different
//! chipset's arch crate supplies whatever *its* boot format needs instead.
//!
//! Placement: the linker puts `.rodata_appdesc` at the very start of the first
//! loaded segment (DRAM, lowest address), so it lands at image offset 0x20.
//! `EXTERN(esp_app_desc)` in the linker script forces it past `--gc-sections`.

/// Layout matches `esp_app_desc_t` (256 bytes). The bootloader only strictly
/// validates `magic_word`; the rest is informational (shown in the boot log).
#[repr(C)]
pub struct EspAppDesc {
    pub magic_word: u32,
    pub secure_version: u32,
    pub reserv1: [u32; 2],
    pub version: [u8; 32],
    pub project_name: [u8; 32],
    pub time: [u8; 16],
    pub date: [u8; 16],
    pub idf_ver: [u8; 32],
    pub app_elf_sha256: [u8; 32],
    pub reserv2: [u32; 20],
}

/// Copy a string literal into a fixed `[u8; N]`, NUL-padded.
const fn cstr<const N: usize>(s: &str) -> [u8; N] {
    let mut out = [0u8; N];
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && i < N - 1 {
        out[i] = b[i];
        i += 1;
    }
    out
}

/// The Espressif magic word for an application descriptor.
const ESP_APP_DESC_MAGIC: u32 = 0xABCD_5432;

#[no_mangle]
#[used]
#[link_section = ".rodata_appdesc"]
pub static esp_app_desc: EspAppDesc = EspAppDesc {
    magic_word: ESP_APP_DESC_MAGIC,
    secure_version: 0,
    reserv1: [0; 2],
    version: cstr("0.1.0"),
    project_name: cstr("flint"),
    time: [0; 16],
    date: [0; 16],
    idf_ver: cstr("none"),
    app_elf_sha256: [0; 32],
    reserv2: [0; 20],
};
