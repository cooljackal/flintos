#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Layer-boundary enforcement (plan W7.1).
#
# Two rules, both whitelists:
#
#   drivers/logical/*, drivers/bus/*   may depend only on api
#   lib/*                              may depend only on api and other lib/*
#
# A driver knows a specific part number and its output is destined for a pin.
# Letting one reach hardware register definitions defeats the three-layer
# model, so api is all it gets.
#
# `lib/*` is not drivers. These are portable libraries that touch no register,
# name no part number, and return values rather than driving anything --
# geometry, framebuffers, colour conversion. They may build on each other,
# because composing them creates no route to hardware: none of them has one to
# begin with. If a lib/ crate ever needs hal or a soc, it was misfiled.
#
# This is a WHITELIST: everything except api is a violation. It used to be a
# blacklist of three prefixes (hal, arch-*, soc-*), which missed the most
# obvious violation of all -- a bus crate depending directly on a Layer-1
# physical driver such as esp32-spi -- and every crates.io dependency besides.
# Naming what is allowed is both shorter and total.
#
# It reads `cargo metadata` rather than grepping manifests, for two reasons:
# resolved package names see through Cargo's rename syntax (a blacklist grep for
# `^\s*hal\s*=` is defeated by `h = { package = "hal", ... }`), and identity
# comes from the package name rather than from where the directory happens to
# sit.
#
# Exit non-zero on any violation. Wired into CI and `make check-layers`.

set -euo pipefail
cd "$(dirname "$0")/.."

# Probe each candidate by running it, rather than trusting `command -v`: Windows
# ships a `python3` shim on PATH that is not an interpreter at all -- it prints
# an advert for the Microsoft Store and exits non-zero.
PY=""
for candidate in python3 python py; do
    if command -v "$candidate" >/dev/null 2>&1 &&
       "$candidate" -c 'import json,sys' >/dev/null 2>&1; then
        PY="$candidate"
        break
    fi
done
[ -n "$PY" ] || {
    echo "check-layers: needs a working python3 (or python) to read cargo metadata" >&2
    exit 1
}

PYSRC=$(cat <<'EOF'
import json, os, sys

# The one dependency a portable driver is allowed to name.
ALLOWED = {"api"}

# Directory -> layer. Layer is a property of the package, but the directory is
# what declares intent, and a crate in the wrong directory is its own bug.
LAYERS = {
    "drivers/logical": "Layer-3 logical driver",
    "drivers/bus":     "Layer-2 bus abstraction",
    "lib":             "portable library",
}

meta = json.load(sys.stdin)

def category(path):
    for d, l in LAYERS.items():
        if "/%s/" % d in path + "/":
            return d, l
    return None, None

# Every lib/ crate, so lib/ crates can be allowed to name each other. Collected
# from the workspace rather than hardcoded: a new one should not need this file
# edited to be usable by its neighbours.
LIBS = {
    pkg["name"]
    for pkg in meta["packages"]
    if category(os.path.dirname(pkg["manifest_path"]).replace("\\", "/"))[0] == "lib"
}
violations = []
checked = 0

for pkg in meta["packages"]:
    path = os.path.dirname(pkg["manifest_path"]).replace("\\", "/")
    directory, layer = category(path)
    if layer is None:
        continue
    checked += 1
    allowed = ALLOWED | LIBS if directory == "lib" else ALLOWED
    rule = ("api and other lib/ crates" if directory == "lib" else "api")
    for dep in pkg["dependencies"]:
        # dev-dependencies are test scaffolding; they never ship in the image
        # and so cannot leak hardware access into a driver's public surface.
        if dep["kind"] == "dev":
            continue
        if dep["name"] not in allowed:
            violations.append(
                "LAYER VIOLATION: %s (%s) depends on %s -- may depend only on %s"
                % (pkg["name"], layer, dep["name"], rule)
            )

for v in violations:
    print(v)

if violations:
    print("")
    print("%d layer violation(s) found. Drivers may depend only on api; "
          "lib/ crates only on api and each other." % len(violations))
    sys.exit(1)

if checked == 0:
    print("check-layers: matched no driver or lib crates -- has the layout moved?")
    sys.exit(1)

print("Layer boundary OK: %d driver/lib crates within their dependency whitelist "
      "(%d lib)." % (checked, len(LIBS)))
EOF
)

cargo metadata --format-version 1 --no-deps | "$PY" -c "$PYSRC"
