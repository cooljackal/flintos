#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0

"""What Espressif's radio blobs need from us (step 3.3 of doc/plan-radio.md).

The issue's bar for this step is that unresolved symbols are "listed, not
discovered one at a time". Iterating on link failures gives them one at a
time -- the linker gives up after the first few dozen, and the order says
nothing about where the work is. Reading the archives directly gives the whole
set at once, before any link is attempted.

Method: every symbol the linked archives reference and none of them define.

## Why this is Python

It began as shell, and the shell version produced a *different answer on every
run*: 133, then 19, then 40, then 165, with no error any time. Three separate
causes, each invisible:

  * `comm` needs both inputs sorted in the same collation, and `sort` collates
    by locale -- so the result depended on the environment `make` happened to
    provide.
  * The Makefile passes a drive-letter path (`C:/Users/...`) that this shell
    cannot stat, so the tool silently fell through to a different `nm` on PATH.
  * `set -o pipefail` turned `find` and `grep` returning "nothing matched" into
    fatal errors in some paths and silent empties in others.

A tool whose answer depends on which shell invoked it is worse than no tool.
Sets and subprocesses have none of those failure modes.
"""

import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BLOBS = ROOT / ".blobs" / "esp32"

# Only the archives actually linked. Counting what the unused ones need would
# overstate the work considerably -- libmesh alone drags in a great deal a
# station build never touches.
LINKED = ["core", "net80211", "pp", "coexist", "phy", "rtc", "wapi"]

# Grouped, because 60 names in one list says nothing about where the work is.
# The prefixes are Espressif's own naming, so the groups line up with the
# subsystems in doc/plan-radio.md.
GROUPS = [
    ("OS adapter", lambda s: s.startswith(("g_wifi_osi", "wifi_osi", "g_osi"))),
    ("PHY and RF", lambda s: s.startswith(("phy_", "rtc_", "bb_", "rf_", "chip_"))),
    ("Coexistence", lambda s: s.startswith("coex")),
    ("Mesh (stubbed)", lambda s: "mesh" in s),
    ("esp_* API", lambda s: s.startswith("esp_")),
    ("ROM", lambda s: s.startswith(("ets_", "rom_", "Cache_"))),
    ("compiler builtins", lambda s: s.startswith("__")),
    ("C library", lambda s: s in {
        "abort", "puts", "printf", "sprintf", "snprintf", "malloc", "free",
        "calloc", "realloc", "memcpy", "memset", "memmove", "memcmp",
        "strcpy", "strlen", "strncmp", "strncpy", "strnlen", "strcmp",
    }),
]


def find_nm() -> str:
    """The Xtensa nm. A host nm reports nothing and exits zero on these."""
    candidates = []
    gcc_dir = os.environ.get("ESP_GCC_DIR", "").strip()
    if gcc_dir:
        # The Makefile hands over a drive-letter path. Python is happy with it
        # where the shell was not, which is half the reason this is Python.
        candidates += [Path(gcc_dir) / "xtensa-esp32-elf-nm",
                       Path(gcc_dir) / "xtensa-esp-elf-nm"]
    try:
        home = subprocess.run(["rustup", "show", "home"], capture_output=True,
                              text=True, timeout=30).stdout.strip()
        if home:
            bin_dir = Path(home) / "toolchains" / "esp" / "xtensa-esp-elf" / "bin"
            candidates += [bin_dir / "xtensa-esp32-elf-nm",
                           bin_dir / "xtensa-esp-elf-nm"]
    except Exception:
        pass

    for c in candidates:
        for path in (c.with_suffix(".exe"), c):
            if path.is_file():
                return str(path)
    found = shutil.which("xtensa-esp32-elf-nm")
    if found:
        return found
    sys.exit("blob-symbols: no Xtensa nm found. Run: make env")


def symbols(nm: str, archive: Path, defined: bool) -> set[str]:
    flag = "--defined-only" if defined else "--undefined-only"
    out = subprocess.run([nm, flag, "--format=posix", str(archive)],
                         capture_output=True, text=True)
    names = set()
    for line in out.stdout.splitlines():
        line = line.strip()
        # posix format lists an archive member as `name.o:` on its own line.
        if not line or line.endswith(":"):
            continue
        parts = line.split()
        if parts:
            names.add(parts[0])
    return names


def main() -> int:
    elf = None
    for arg in sys.argv[1:]:
        if arg in ("-h", "--help"):
            print(__doc__)
            return 0
        elf = arg

    if not BLOBS.is_dir():
        sys.exit(f"blob-symbols: {BLOBS} not found. Run: make blobs")

    nm = find_nm()
    needed: set[str] = set()
    provided: set[str] = set()
    per_archive = []
    for name in LINKED:
        archive = BLOBS / f"lib{name}.a"
        if not archive.is_file():
            continue
        u = symbols(nm, archive, defined=False)
        d = symbols(nm, archive, defined=True)
        needed |= u
        provided |= d
        per_archive.append((name, len(d), len(u)))

    # An nm that ran but read nothing reports "0 unresolved", which reads as
    # "there is no work to do" and is the most dangerous output here. It has
    # already happened once, from a path the shell could not stat.
    if not provided:
        sys.exit(f"blob-symbols: {nm} produced no symbols at all.\n"
                 f"  These archives define thousands, so this is a broken\n"
                 f"  toolchain path rather than an empty result.")

    unresolved = needed - provided

    if elf:
        ours = symbols(nm, Path(elf), defined=True)
        unresolved -= ours

    print(f"Symbols the linked blobs need and no blob defines: {len(unresolved)}")
    if elf:
        print(f"(after subtracting what {elf} already defines)")
    print()

    remaining = set(unresolved)
    for label, matches in GROUPS:
        hits = sorted(s for s in remaining if matches(s))
        if not hits:
            continue
        remaining -= set(hits)
        print(f"  {label:<22} {len(hits):>3}")
        for h in hits[:10]:
            print(f"      {h}")
        if len(hits) > 10:
            print(f"      ... and {len(hits) - 10} more")
        print()
    if remaining:
        print(f"  {'everything else':<22} {len(remaining):>3}")
        for h in sorted(remaining):
            print(f"      {h}")
        print()

    print("Per archive:")
    for name, d, u in per_archive:
        print(f"  lib{name + '.a':<14} defines {d:>5}  needs {u:>5}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
