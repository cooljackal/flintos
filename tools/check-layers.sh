#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Layer-boundary enforcement (plan W7.1).
#
# Layer-3 (drivers/logical/*) and Layer-2 (drivers/bus/*) crates may depend ONLY
# on api. Any other dependency gives a device or bus driver a route to hardware
# register definitions, defeating the three-layer model. This is the structural
# boundary the design promises -- enforce it here so it cannot silently rot.
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
}

meta = json.load(sys.stdin)
violations = []
checked = 0

for pkg in meta["packages"]:
    path = os.path.dirname(pkg["manifest_path"]).replace("\\", "/")
    layer = next((l for d, l in LAYERS.items() if "/%s/" % d in path + "/"), None)
    if layer is None:
        continue
    checked += 1
    for dep in pkg["dependencies"]:
        # dev-dependencies are test scaffolding; they never ship in the image
        # and so cannot leak hardware access into a driver's public surface.
        if dep["kind"] == "dev":
            continue
        if dep["name"] not in ALLOWED:
            violations.append(
                "LAYER VIOLATION: %s (%s) depends on %s -- may depend only on api"
                % (pkg["name"], layer, dep["name"])
            )

for v in violations:
    print(v)

if violations:
    print("")
    print("%d layer violation(s) found. Logical and bus drivers must depend "
          "only on api." % len(violations))
    sys.exit(1)

if checked == 0:
    print("check-layers: matched no logical/bus crates -- has the layout moved?")
    sys.exit(1)

print("Layer boundary OK: %d logical/bus crates depend only on api." % checked)
EOF
)

cargo metadata --format-version 1 --no-deps | "$PY" -c "$PYSRC"
