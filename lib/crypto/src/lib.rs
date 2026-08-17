// SPDX-License-Identifier: Apache-2.0

//! First-party `no_std` cryptographic primitives.
//!
//! These exist for one reason: WPA2/WPA3 association. Espressif's radio blobs
//! drive the MAC, but the 4-way handshake's crypto is not in them — esp-idf
//! supplies it from mbedTLS, backed by the ESP32's SHA and AES hardware
//! accelerators. FlintOS links no external crates and vendors no SDK, so the
//! primitives the adapter's `wpa_crypto_funcs` table needs are implemented
//! here, the same way [`heap`](../heap) and [`kvstore`](../kvstore) are.
//!
//! # Software first, hardware behind the same shape
//!
//! Each core here is a plain software implementation, tested against the
//! published vectors — FIPS 180-4 for the hashes, and later NIST and RFC
//! vectors for AES and the compositions. That is deliberate discipline, not a
//! placeholder: when the SHA and AES accelerator drivers land (#33), they slot
//! in behind the same interface, and the software version stays as the thing
//! the hardware is checked against. A hardware bug then cannot be mistaken for
//! a handshake bug, because the software path already passed the vectors.
//!
//! # Not constant-time everywhere, and where that matters
//!
//! WPA2-PSK's secrets are the PMK and PTK. The compositions that touch them —
//! HMAC, the key derivation — must not branch or index on secret data. The
//! hashes here do not (SHA is data-independent by construction). AES and the
//! CCM/CMAC layers, when they arrive, will need the same care and it will be
//! stated at each one rather than assumed.

#![no_std]
#![forbid(unsafe_code)]

pub mod sha256;

pub use sha256::Sha256;
