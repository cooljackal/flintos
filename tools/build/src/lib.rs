// SPDX-License-Identifier: Apache-2.0

//! Build-script helper for FlintOS applications.
//!
//! A FlintOS application is an ordinary `no_std` binary crate, but it links
//! against a linker script that lays out ESP32 IRAM, DRAM, the task stack pool
//! and the vector table. Cargo does not propagate `rustc-link-arg` from a
//! dependency's build script to the binary that depends on it, so every
//! application needs a build script of its own. This makes that build script
//! one line:
//!
//! ```ignore
//! // apps/<name>/build.rs
//! fn main() {
//!     build::link();
//! }
//! ```

mod blobs;
pub mod map;

use std::path::{Path, PathBuf};

use map::DramMap;

/// Relative path of the linker script from the workspace root.
const LD_SCRIPT: &str = "arch/xtensa/flint32.ld";

/// The ROM address table the linker script includes. Named here so a change to
/// it rebuilds, which it would not otherwise -- the generated script is what
/// cargo watches, and this is pulled in by the linker rather than by us.
const ROM_SCRIPT: &str = "esp32.rom.ld";

/// Markers around the `MEMORY` entries this crate rewrites.
const MAP_BEGIN: &str = "/* @FLINT-DRAM-MAP-BEGIN */";
const MAP_END: &str = "/* @FLINT-DRAM-MAP-END */";

/// Where the resolved script is left for `make size` to read.
///
/// `OUT_DIR` is the linker's copy, but its path contains a build hash that
/// nothing outside Cargo can predict, and reporting sizes against the
/// unresolved template would quietly show the wrong capacities for a
/// Bluetooth build. So the same text is written somewhere findable.
const GENERATED: &str = "target/flint32.generated.ld";

/// Pass FlintOS's linker script to the final link of the calling binary.
///
/// Call this from an application's `build.rs`. Does nothing on host targets, so
/// `cargo test` and `cargo check` against the host toolchain still work.
pub fn link() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("xtensa") {
        return;
    }

    let script = find_ld_script().unwrap_or_else(|| {
        panic!(
            "build: could not find {LD_SCRIPT} in any ancestor of \
             CARGO_MANIFEST_DIR. An application must live inside the FlintOS \
             workspace, or supply its own linker script instead of calling \
             build::link()."
        )
    });

    // `CARGO_FEATURE_*` here is the *application's* feature set: this function
    // is called from the app's own build script, so the radio features an app
    // forwards to the kernel are visible. That is what keeps the memory map
    // and the kernel build from disagreeing.
    let bluetooth = std::env::var_os("CARGO_FEATURE_RADIO_BT").is_some();
    let dram = DramMap::new(bluetooth);
    if let Some(problem) = dram.problem() {
        panic!("build: the DRAM map does not fit: {problem}");
    }

    let template = std::fs::read_to_string(&script)
        .unwrap_or_else(|e| panic!("build: cannot read {}: {e}", script.display()));
    let resolved = substitute(&template, &dram).unwrap_or_else(|e| {
        panic!("build: cannot generate the memory map from {}: {e}", script.display())
    });

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set for build scripts"))
        .join("flint32.ld");
    std::fs::write(&out, &resolved)
        .unwrap_or_else(|e| panic!("build: cannot write {}: {e}", out.display()));

    // Best-effort: `make size` prefers this, and falls back to the template.
    // A failure here must not break the build, since it is only reporting.
    if let Some(root) = workspace_root(&script) {
        let findable = root.join(GENERATED);
        if let Some(dir) = findable.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&findable, &resolved);
    }

    // The generated script lives in OUT_DIR, so its `INCLUDE esp32.rom.ld`
    // cannot resolve relative to the source tree. Adding the script's own
    // directory to the search path is what makes the include findable.
    if let Some(dir) = script.parent() {
        println!("cargo:rustc-link-arg=-L{}", dir.display());
        println!("cargo:rerun-if-changed={}", dir.join(ROM_SCRIPT).display());
    }
    println!("cargo:rustc-link-arg=-T{}", out.display());
    println!("cargo:rerun-if-changed={}", script.display());

    // Espressif's archives, for an application that has asked for them. Same
    // reason this function exists at all: the flags have to come from the
    // build script of the binary being linked.
    blobs::link();
}

/// Replace the marked `MEMORY` entries with the computed map.
///
/// Fails rather than falling back. A missing marker means someone edited the
/// linker script in a way this cannot follow, and silently linking the
/// template's default map into a Bluetooth build would put static data
/// underneath the radio controller — which works until the controller starts.
fn substitute(template: &str, dram: &DramMap) -> Result<String, String> {
    let begin = template
        .find(MAP_BEGIN)
        .ok_or_else(|| format!("marker {MAP_BEGIN} not found"))?;
    let end = template
        .find(MAP_END)
        .ok_or_else(|| format!("marker {MAP_END} not found"))?;
    if end < begin {
        return Err(format!("{MAP_END} appears before {MAP_BEGIN}"));
    }

    let mut s = String::with_capacity(template.len() + 256);
    s.push_str(&template[..begin]);
    s.push_str(MAP_BEGIN);
    s.push_str("\n  /* Generated by tools/build::link(). Do not edit here. */\n");
    s.push_str(&dram.to_linker_text());
    s.push_str(&template[end..]);
    Ok(s)
}

/// The directory containing `arch/`, given the path to the linker script.
fn workspace_root(script: &Path) -> Option<PathBuf> {
    // <root>/arch/xtensa/flint32.ld -> <root>
    script.parent()?.parent()?.parent().map(PathBuf::from)
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The real linker script, so these test the file that actually ships
    /// rather than a fixture that can drift away from it.
    fn template() -> String {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("tools/build sits two levels below the workspace root")
            .to_path_buf();
        std::fs::read_to_string(root.join(LD_SCRIPT)).expect("the linker script is readable")
    }

    #[test]
    fn the_shipped_script_still_has_its_markers() {
        // If someone reformats the MEMORY block and drops these, every build
        // starts failing with the message from `substitute`. This test says so
        // on a laptop first.
        let t = template();
        assert!(t.contains(MAP_BEGIN), "{MAP_BEGIN} missing");
        assert!(t.contains(MAP_END), "{MAP_END} missing");
        assert!(t.find(MAP_BEGIN) < t.find(MAP_END));
    }

    /// Just the text between the markers — the part this crate generates.
    /// The surrounding prose documents both maps, so matching on the whole
    /// file would find words the generator never wrote.
    fn generated_block(out: &str) -> &str {
        let begin = out.find(MAP_BEGIN).expect("begin marker");
        let end = out.find(MAP_END).expect("end marker");
        &out[begin..end]
    }

    #[test]
    fn substituting_the_default_map_preserves_every_address() {
        // The template's committed numbers are the no-Bluetooth map, so
        // regenerating it must produce the same ORIGINs. This is what lets
        // `make size` parse the template directly and still be right.
        let out = substitute(&template(), &DramMap::new(false)).expect("substitution works");
        for want in [
            "ORIGIN = 0x3FFB0000",
            "ORIGIN = 0x3FFC0000",
            "ORIGIN = 0x3FFD8000",
            "ORIGIN = 0x3FFD9000",
        ] {
            assert!(out.contains(want), "{want} missing from generated script");
        }
        assert!(!generated_block(&out).contains("bt_reserve"));
    }

    #[test]
    fn substituting_the_bluetooth_map_moves_everything_up() {
        let out = substitute(&template(), &DramMap::new(true)).expect("substitution works");
        assert!(out.contains("ORIGIN = 0x3FFBE000"), "dram_seg should start past the reservation");
        assert!(
            generated_block(&out).contains("bt_reserve"),
            "the reservation should be documented in the output"
        );
        // The default origin must be gone, or both maps are present and the
        // linker takes whichever it sees last.
        assert!(!generated_block(&out).contains("ORIGIN = 0x3FFB0000,"));
    }

    #[test]
    fn the_rest_of_the_script_is_untouched() {
        // Only the marked block may change: the SECTIONS body, the ASSERTs and
        // the flash regions must survive verbatim.
        let t = template();
        let out = substitute(&t, &DramMap::new(true)).expect("substitution works");
        for keep in [
            "_kernel_stack_start",
            "_dma_pool_start",
            "irom_seg",
            "0x3FFDC200",
        ] {
            assert!(out.contains(keep), "{keep} lost during substitution");
        }
    }

    #[test]
    fn a_missing_marker_is_an_error_and_not_a_silent_default() {
        // The dangerous failure: emitting the template's map into a Bluetooth
        // build puts static data underneath the radio controller.
        let err = substitute("MEMORY { }", &DramMap::new(true)).expect_err("should fail");
        assert!(err.contains("not found"), "{err}");
    }
}
