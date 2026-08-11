// SPDX-License-Identifier: Apache-2.0

//! Point the linker at Espressif's radio archives.
//!
//! # Why this lives in the application's build script
//!
//! It used to live in `radio/esp32/build.rs`, next to the crate that declares
//! the `extern "C"` functions, which is where it reads as belonging. It does
//! not work there, for the reason this crate's own module docs already state:
//!
//! > Cargo does not propagate `rustc-link-arg` from a dependency's build
//! > script to the binary that depends on it.
//!
//! `cargo:rustc-link-search` *does* propagate, so the failure was not obvious
//! — the linker was given the directory and none of the `-l` flags, and
//! reported five undefined PHY symbols as though the archives were missing.
//!
//! `cargo:rustc-link-lib` would propagate, but cannot express the one thing
//! these archives need: `--start-group`. So the flags are emitted here, from
//! the build script of the binary being linked, which is the only place they
//! reach the link at all.
//!
//! # When it runs
//!
//! Only when the application enables its own `blobs` feature, and only for the
//! Xtensa target. `CARGO_FEATURE_BLOBS` in *this* build script is the
//! application's feature, exactly as `CARGO_FEATURE_RADIO_BT` is for the
//! memory map.
//!
//! The archives are not in the repository. `make blobs` fetches them into
//! `.blobs/`, pinned to the revisions esp-idf references; see
//! `tools/fetch-blobs.sh` and `doc/plan-radio.md`.

use std::path::{Path, PathBuf};

/// Where `tools/fetch-blobs.sh` puts them, relative to the workspace root.
const BLOB_DIR: &str = ".blobs/esp32";

/// The archives, in the order the linker wants them.
///
/// Order matters for `ar` archives: the linker takes one pass and resolves
/// only what is already undefined, so a library must come *after* whatever
/// needs it. `pp` and `net80211` reference each other, which no ordering can
/// satisfy on a single pass — `--start-group` is what handles that, and it is
/// why these are emitted as a group rather than as plain `-l` flags.
const ARCHIVES: &[&str] = &["core", "net80211", "pp", "coexist", "phy", "rtc", "wapi"];

/// Fetched but not linked, with the reason. Reported once at build time so the
/// choice is visible rather than buried here -- 1.2 MB of the download is
/// archives a station-plus-BLE build never references.
const NOT_LINKED: &[(&str, &str)] = &[
    // Not merely unwanted: `libnet80211.a` references thirteen mesh symbols
    // unconditionally, so leaving this out is not free -- they have to be
    // stubbed. Linking it instead resolves those thirteen but pulls in seven
    // more, including esp_event_handler_register, and FlintOS has no event
    // loop to hang that on. Stubs are the smaller surface. See step 3.3 in
    // doc/plan-radio.md.
    ("mesh", "mesh is a non-goal; its 13 referenced symbols are stubbed instead"),
    ("espnow", "ESP-NOW is a non-goal"),
    ("smartconfig", "provisioning over the air; not needed to associate"),
    ("btdm_app", "BLE controller, linked by #66 rather than here"),
];

/// Emit the link flags for Espressif's archives, if this build wants them.
///
/// Called from [`crate::link`], after the target has been checked.
pub(crate) fn link() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_BLOBS");

    // Track the directory itself, so removing the archives re-runs this and
    // produces the message below rather than a cached set of link flags
    // pointing at nothing -- which surfaces much later as `cannot find -lpp`
    // and names neither the cause nor the fix. Emitted whether or not it
    // currently exists, so creating it also triggers a rebuild.
    if let Some(root) = workspace_root() {
        println!("cargo:rerun-if-changed={}", root.join(BLOB_DIR).display());
    }

    if std::env::var_os("CARGO_FEATURE_BLOBS").is_none() {
        return;
    }

    let dir = match find_blobs() {
        Some(d) => d,
        None => panic!(
            "\n\nbuild: the `blobs` feature is enabled but Espressif's \
             archives are not present.\n\n    make blobs\n\nfetches them \
             (~4.3 MB, Apache-2.0, pinned to esp-idf v4.4) into {BLOB_DIR}. \
             They are deliberately not committed -- see doc/plan-radio.md for \
             why fetching beats vendoring.\n"
        ),
    };

    let missing: Vec<&str> = ARCHIVES
        .iter()
        .copied()
        .filter(|name| !dir.join(format!("lib{name}.a")).is_file())
        .collect();
    if !missing.is_empty() {
        panic!(
            "\n\nbuild: {} is present but incomplete -- missing: {}\n\n    \
             make blobs\n\nwill re-fetch. If it persists, the pinned revisions \
             in tools/fetch-blobs.sh may no longer carry these archives.\n",
            dir.display(),
            missing.join(", ")
        );
    }

    println!("cargo:rustc-link-arg=-L{}", dir.display());

    // A group, not a sequence: `libpp.a` and `libnet80211.a` reference each
    // other, so no single-pass ordering resolves both. Without this the link
    // fails with undefined symbols that exist in an archive the linker has
    // already walked past, which reads as a missing library rather than an
    // ordering problem.
    println!("cargo:rustc-link-arg=-Wl,--start-group");
    for name in ARCHIVES {
        println!("cargo:rustc-link-arg=-l{name}");
    }
    println!("cargo:rustc-link-arg=-Wl,--end-group");

    for (name, why) in NOT_LINKED {
        println!("cargo:warning=build: lib{name}.a fetched but not linked ({why})");
    }
}

/// Walk up from the calling crate looking for the workspace's blob directory.
///
/// Same approach as [`crate::link`]'s search for the linker script: it means
/// an application does not have to know how deep it sits.
fn find_blobs() -> Option<PathBuf> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").ok()?);
    manifest
        .ancestors()
        .map(|d| d.join(BLOB_DIR))
        .find(|c| c.is_dir())
        .filter(|d| has_any_archive(d))
}

/// The workspace root: the first ancestor with a `tools/` directory beside a
/// `Cargo.toml`. Used to name the blob path even when it does not yet exist.
fn workspace_root() -> Option<PathBuf> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").ok()?);
    manifest
        .ancestors()
        .find(|d| d.join("tools").is_dir() && d.join("Cargo.toml").is_file())
        .map(PathBuf::from)
}

fn has_any_archive(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|e| {
                e.path()
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("a"))
            })
        })
        .unwrap_or(false)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_linked_archive_is_named_once() {
        // A duplicate is not harmless inside a group: it is a second scan of
        // the same members, and it hides an ordering mistake rather than
        // fixing it.
        for (i, a) in ARCHIVES.iter().enumerate() {
            assert!(!ARCHIVES[i + 1..].contains(a), "{a} listed twice");
        }
    }

    #[test]
    fn nothing_is_both_linked_and_deliberately_skipped() {
        // The two lists are the whole of what `make blobs` fetches, split by
        // a decision. An archive in both would mean the recorded reason for
        // skipping it is a lie.
        for (name, _) in NOT_LINKED {
            assert!(!ARCHIVES.contains(name), "{name} is linked and skipped");
        }
    }

    #[test]
    fn the_group_is_what_makes_the_pair_resolvable() {
        // `pp` and `net80211` reference each other. If a refactor ever drops
        // one of them from the group the link breaks in a way that reads as a
        // missing library, so pin the fact that both are in the list.
        assert!(ARCHIVES.contains(&"pp"));
        assert!(ARCHIVES.contains(&"net80211"));
    }
}
