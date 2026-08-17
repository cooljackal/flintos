// SPDX-License-Identifier: Apache-2.0

//! A first-party WPA2/WPA3-Personal supplicant: the 4-way handshake.
//!
//! When a station joins a protected network, the AP and the station run a
//! four-message exchange that proves both know the pre-shared key and
//! establishes the session keys, without ever sending the key. This crate is
//! the station's half of that exchange, in first-party `no_std` Rust over
//! [`crypto`](../crypto).
//!
//! # Pure protocol, no radio
//!
//! The supplicant is deliberately radio-blind. It consumes EAPOL-Key frames as
//! bytes and produces response frames and derived key material as bytes — it
//! never sends a frame or installs a key itself. That is what keeps it
//! portable across every FlintOS target and fully testable on a host against a
//! captured handshake: the same state machine that runs on the ESP32 blob
//! radio runs unchanged over any other, because it knows about neither.
//!
//! A radio backend drives it: hand each received EAPOL frame to the supplicant,
//! send whatever response it returns, and install the keys it derives when it
//! reports the handshake complete.
//!
//! # Scope
//!
//! WPA2-Personal (PSK) with CCMP first — key descriptor version 2, HMAC-SHA1
//! MIC, AES-key-wrapped group key. WPA3-SAE and the older TKIP/version-1 paths
//! are named where they would slot in rather than half-built, the same
//! discipline `crypto` follows.
//!
//! This crate is under construction; the modules below fill in as the handshake
//! is built, each against its own test vectors.

#![no_std]
#![forbid(unsafe_code)]

pub mod eapol;
pub mod handshake;
pub mod keydata;
pub mod rsn;

pub use handshake::{Action, Rng, State, Supplicant};
