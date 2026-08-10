#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Fetch Espressif's radio blobs (step 3.2 of doc/plan-radio.md, issue #65).
#
# Wi-Fi and BLE on the ESP32 are not open source. Espressif ships them as
# precompiled archives, and every RTOS that supports these radios links the
# same files -- NuttX, Zephyr and Arduino included. They are Apache-2.0, the
# same licence as FlintOS, so this is not a licensing workaround; see
# doc/plan-radio.md for the research.
#
# ## Fetched, not vendored
#
# 4.2 MB of binaries in git history is permanent: every clone pays it forever
# and every update pays again. Fetching also means FlintOS never redistributes
# somebody else's binaries, so the attribution obligations stay with Espressif
# where they already are. NuttX and Zephyr both do it this way.
#
# ## Integrity
#
# By commit, not by checksum. Git already content-addresses everything it
# stores, so checking out an exact commit *is* the checksum -- and a separate
# list of hashes would be one more thing to keep in step. The pins below are
# the submodule revisions esp-idf itself references at the release named in
# IDF_REF, which is the same release radio/esp32/src/osi.rs was generated from.
# Those two must move together: the OS adapter table is version-checked by the
# blob at init, and a mismatch is the failure mode issue #65 calls "a working
# radio that corrupts memory".

set -euo pipefail
cd "$(dirname "$0")/.."

# The esp-idf release everything here is pinned to. Changing this means
# regenerating the OSI table as well -- see radio/esp32/src/osi.rs.
IDF_REF="v4.4"

# repo|commit|subdirectory holding the ESP32 archives
BLOBS=(
    "esp32-wifi-lib|cd7d14917f2c3d0ea4382f4a188cb290304faf47|esp32"
    "esp-phy-lib|2d89c532ccba0bb9988d1d1c6d719bbe1d8b65b8|esp32"
    "esp32-bt-lib|54a69e53616cbd3e3f3bbf150e42930a7912349a|esp32"
)

# Deliberately outside target/: `make clean` should not cost a 4 MB download.
DEST=".blobs"
CACHE="$DEST/.git-cache"

usage() {
    cat <<'USAGE'
usage: fetch-blobs.sh [--check]

  (no arguments)  fetch the archives into .blobs/esp32/
  --check         report what is present and exit non-zero if anything is not

The archives are Apache-2.0 and come from Espressif's own repositories, pinned
to the revisions esp-idf references. See doc/plan-radio.md.
USAGE
}

CHECK_ONLY=0
case "${1:-}" in
    --check) CHECK_ONLY=1 ;;
    -h|--help) usage; exit 0 ;;
    "") ;;
    *) usage >&2; exit 2 ;;
esac

command -v git >/dev/null 2>&1 || {
    echo "fetch-blobs: needs git on PATH" >&2
    exit 1
}

if [ "$CHECK_ONLY" = 1 ]; then
    # `|| true` on the count: with `set -o pipefail`, find's non-zero exit on a
    # missing directory takes the whole script down before it can say anything,
    # which is how this first reported "not fetched" by printing nothing at all.
    n=0
    if [ -d "$DEST/esp32" ]; then
        n=$(find "$DEST/esp32" -name '*.a' 2>/dev/null | wc -l | tr -d ' ' || true)
    fi
    if [ "${n:-0}" -eq 0 ]; then
        echo "blobs: not fetched. Run: make blobs"
        exit 1
    fi
    echo "blobs: $n archives present in $DEST/esp32 (esp-idf $IDF_REF pins)"
    exit 0
fi

mkdir -p "$DEST/esp32" "$CACHE"

echo "Fetching Espressif radio blobs, pinned to esp-idf $IDF_REF."
echo "These are Apache-2.0 binaries from Espressif; see doc/plan-radio.md."
echo

for entry in "${BLOBS[@]}"; do
    IFS='|' read -r repo commit subdir <<<"$entry"
    work="$CACHE/$repo"

    if [ ! -d "$work/.git" ]; then
        # An empty repository plus a single-commit fetch: cloning the default
        # branch would pull every chip family's archives and their history,
        # which is an order of magnitude more than is wanted.
        git init -q "$work"
        git -C "$work" remote add origin "https://github.com/espressif/$repo.git"
    fi

    have=$(git -C "$work" rev-parse --verify --quiet HEAD 2>/dev/null || true)
    if [ "$have" != "$commit" ]; then
        echo "  $repo -> ${commit:0:12}"
        if ! git -C "$work" fetch -q --depth 1 origin "$commit"; then
            echo "fetch-blobs: could not fetch $commit from $repo." >&2
            echo "  Check network access to github.com, or that the pin is still" >&2
            echo "  reachable -- a force-push upstream would orphan it." >&2
            exit 1
        fi
        git -C "$work" checkout -q FETCH_HEAD
    else
        echo "  $repo already at ${commit:0:12}"
    fi

    if [ ! -d "$work/$subdir" ]; then
        echo "fetch-blobs: $repo has no '$subdir' directory at this pin." >&2
        exit 1
    fi
    cp "$work/$subdir"/*.a "$DEST/esp32/"

    # Apache-2.0 asks that the licence travel with the work. It costs nothing
    # to keep it beside the archives rather than only in a document.
    if [ -f "$work/LICENSE" ]; then
        cp "$work/LICENSE" "$DEST/esp32/LICENSE.$repo"
    fi
done

count=$(find "$DEST/esp32" -name '*.a' | wc -l | tr -d ' ')
size=$(du -sh "$DEST/esp32" 2>/dev/null | cut -f1)
echo
echo "$count archives in $DEST/esp32 ($size), with each repository's LICENSE."
echo "Build against them with:  make build APP=<app> EXTRA_FEATURES=radio-wifi"
