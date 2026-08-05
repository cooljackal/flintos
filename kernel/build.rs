// SPDX-License-Identifier: Apache-2.0

fn main() {
    // Pass the linker script to the final linker invocation.
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ld_script = manifest
        .parent()
        .unwrap()
        .join("arch")
        .join("flint-arch-xtensa")
        .join("flint32.ld");
    println!("cargo:rustc-link-arg=-T{}", ld_script.display());
}
