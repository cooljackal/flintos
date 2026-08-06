// SPDX-License-Identifier: Apache-2.0

//! Build-script helper for Flint applications.
//!
//! A Flint application is an ordinary `no_std` binary crate, but it links
//! against a linker script that lays out ESP32 IRAM, DRAM, the task stack pool
//! and the vector table. Cargo does not propagate `rustc-link-arg` from a
//! dependency's build script to the binary that depends on it, so every
//! application needs a build script of its own. This makes that build script
//! one line:
//!
//! ```ignore
//! // apps/<name>/build.rs
//! fn main() {
//!     flint_build::link();
//! }
//! ```

use std::path::PathBuf;

/// Relative path of the linker script from the workspace root.
const LD_SCRIPT: &str = "arch/flint-arch-xtensa/flint32.ld";

/// Pass Flint's linker script to the final link of the calling binary.
///
/// Call this from an application's `build.rs`. Does nothing on host targets, so
/// `cargo test` and `cargo check` against the host toolchain still work.
pub fn link() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("xtensa") {
        return;
    }

    let script = find_ld_script().unwrap_or_else(|| {
        panic!(
            "flint-build: could not find {LD_SCRIPT} in any ancestor of \
             CARGO_MANIFEST_DIR. An application must live inside the Flint \
             workspace, or supply its own linker script instead of calling \
             flint_build::link()."
        )
    });

    println!("cargo:rustc-link-arg=-T{}", script.display());
    println!("cargo:rerun-if-changed={}", script.display());
}

/// Walk up from the calling crate's directory looking for the linker script.
///
/// Searching rather than hardcoding `../../arch/...` means an application can
/// sit at any depth — `apps/hello/`, `apps/vendor/thing/`, or a directory of
/// the user's own — without its build script needing to know how deep it is.
fn find_ld_script() -> Option<PathBuf> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").ok()?);
    manifest
        .ancestors()
        .map(|dir| dir.join(LD_SCRIPT))
        .find(|candidate| candidate.is_file())
}
