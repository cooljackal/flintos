// SPDX-License-Identifier: Apache-2.0

//! The ESP32's SHA and AES hardware accelerators.
//!
//! These are the hardware behind the shapes in [`crypto`](../../../../lib/crypto):
//! [`sha`] produces SHA-1 and SHA-256 digests, [`aes`] does single-block
//! AES-128/256 ECB. The software lib stays as the thing they are checked
//! against — the on-target self-test runs both paths over the same input and
//! asserts byte-for-byte equality, so a hardware bug cannot hide behind a
//! passing protocol test (see #33 and the `crypto` crate docs).
//!
//! # Both blocks live in the DPORT window
//!
//! `DR_REG_AES_BASE` (`0x3FF0_1000`) and `DR_REG_SHA_BASE` (`0x3FF0_3000`) are
//! inside the DPORT address range, which has two consequences a driver here
//! must respect and neither is optional:
//!
//! - **Reads** of these registers — the digest, the AES output, the busy
//!   flag — take the erratum-safe [`dport::read`], not a plain volatile load.
//!   A raw read can return an unrelated APB value when the other core touches
//!   the bus (soc-esp32 `dport` docs, bug #56). Writes are plain, exactly as
//!   esp-idf's `DPORT_REG_WRITE`/`esp_dport_access_read_buffer` split says.
//! - The block's **clock** is gated by `DPORT_PERI_CLK_EN`, a *different*
//!   register from the `PERIP_CLK_EN` the rest of the chip uses. That is why
//!   this calls [`dport::enable_crypto`], not `dport::enable`.
//!
//! # Byte order is the classic gotcha
//!
//! The two blocks disagree, and getting it wrong yields a digest that is a
//! byte-swapped version of the right answer — plausible enough to pass a
//! glance and fail every vector. Each module states its own convention where
//! it writes and reads the registers; both are pinned by the cross-check
//! against the software lib, which is the only test that can catch a swap.
//!
//! References, read rather than recalled: esp-idf
//! `hal/esp32/include/hal/sha_ll.h` + `aes_ll.h`, `hal/sha_hal.c` +
//! `aes_hal.c`, and `soc/esp32/include/soc/hwcrypto_reg.h`.

#![no_std]

pub mod aes;
pub mod sha;

pub use aes::{Aes, AesKey};
pub use sha::{sha1, sha256, ShaKind};

/// A crypto transform could not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// The accelerator did not go idle within the poll bound. On working
    /// silicon this cannot happen for a single block; it means the clock never
    /// came up or the block is wedged.
    Timeout,
}
