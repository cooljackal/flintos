// SPDX-License-Identifier: Apache-2.0

//! The kernel is a library, so nothing links here — the linker script is
//! supplied by each application's own build script (`flint_build::link()`).
//! This build script exists only to rebuild the kernel when the memory map
//! changes, since a change there invalidates the addresses baked into
//! `boot.rs`'s diagnostics and the stack-pool bounds.

fn main() {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ld_script = manifest
        .parent()
        .unwrap()
        .join("arch")
        .join("xtensa")
        .join("flint32.ld");
    println!("cargo:rerun-if-changed={}", ld_script.display());
}
