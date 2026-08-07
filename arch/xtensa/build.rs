// SPDX-License-Identifier: Apache-2.0

//! Assemble the Xtensa assembly sources with the GNU assembler.
//!
//! These files were previously pulled in with `global_asm!(include_str!(..))`,
//! which routes them through LLVM's integrated assembler. That assembler
//! rejects the windowed-register instructions the exception vectors are built
//! from -- `s32e`, `l32e`, `rfwo`, `rfwu` -- with "instruction use requires an
//! option to be enabled", and no `-C target-feature=+windowed` makes it accept
//! them. The result was a crate that could not be compiled at all for its only
//! supported target.
//!
//! `xtensa-esp32-elf-gcc` assembles all of them without complaint, so the
//! assembly is built here into a static archive and linked whole.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Assembly translation units, relative to the crate root.
const ASM_SOURCES: &[&str] = &[
    "src/asm/vectors.S",
    "src/asm/context.S",
    "src/asm/appcpu.S",
    "src/startup.S",
];

fn main() {
    let target = env::var("TARGET").unwrap_or_default();

    println!("cargo:rerun-if-changed=build.rs");
    for src in ASM_SOURCES {
        println!("cargo:rerun-if-changed={src}");
    }
    println!("cargo:rerun-if-env-changed=XTENSA_GCC");

    // Only the Xtensa target has any use for this. Host builds (docs, tests of
    // the pure-Rust parts) skip it rather than failing for want of a
    // cross-assembler.
    if !target.starts_with("xtensa-") {
        println!(
            "cargo:warning=arch-xtensa: target is `{target}`, skipping assembly build"
        );
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));

    let gcc = env::var("XTENSA_GCC").unwrap_or_else(|_| "xtensa-esp32-elf-gcc".to_string());
    let ar = env::var("XTENSA_AR").unwrap_or_else(|_| "xtensa-esp32-elf-ar".to_string());

    let mut objects = Vec::new();
    for src in ASM_SOURCES {
        let src_path = manifest_dir.join(src);
        let stem = Path::new(src)
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("assembly source has no file stem");
        let obj = out_dir.join(format!("{stem}.o"));

        let status = Command::new(&gcc)
            .arg("-c")
            // Match the code model the Rust side is built with, so the
            // assembler sizes call and literal relocations the same way.
            .arg("-mlongcalls")
            .arg("-mtext-section-literals")
            .arg("-o")
            .arg(&obj)
            .arg(&src_path)
            .status()
            .unwrap_or_else(|e| {
                panic!(
                    "failed to run `{gcc}`: {e}\n\
                     The Xtensa toolchain must be on PATH. Install it with `espup install`, \
                     then add ~/.rustup/toolchains/esp/xtensa-esp-elf/bin to PATH, or set \
                     XTENSA_GCC to the assembler's full path."
                )
            });

        assert!(status.success(), "assembling {src} failed");
        objects.push(obj);
    }

    // Bundle into an archive and link it whole. Without +whole-archive the
    // linker would pull only the members satisfying an already-referenced
    // symbol, and the exception vectors are referenced by nothing -- the CPU
    // reaches them through VECBASE, not through a call -- so they would be
    // silently dropped from the image.
    let archive = out_dir.join("libflintasm.a");
    let _ = std::fs::remove_file(&archive);

    let status = Command::new(&ar)
        .arg("crs")
        .arg(&archive)
        .args(&objects)
        .status()
        .unwrap_or_else(|e| panic!("failed to run `{ar}`: {e}"));
    assert!(status.success(), "archiving the assembly objects failed");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static:+whole-archive=flintasm");
}
