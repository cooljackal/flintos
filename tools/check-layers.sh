#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Layer-boundary enforcement (plan W7.1).
#
# One whitelist per tier. The rule the tiers encode:
#
#   hal                  nothing            contracts only
#   arch/*               hal                the CPU core: traps, context, core timer
#   soc/*                hal                chip infrastructure every peripheral needs
#   drivers/physical/*   hal, soc/*         one peripheral's registers
#   drivers/bus/*        api, lib/*         transport
#   drivers/logical/*    api, lib/*         one part number
#   lib/*                lib/*              no hardware at all
#
# arch and soc hold what is specific to a CPU core or a chip *and shared*.
# Anything that is one peripheral is a driver, wherever it happens to sit
# today: RMT, the watchdogs and the RNG were modules of soc-esp32 and are
# drivers now. The test is whether a second peripheral driver would want it --
# an address map and a pin router yes, a pulse generator no.
#
# `lib/*` is not drivers at all: no registers, no part numbers, values rather
# than anything destined for a pin. It gets no `api`, because api re-exports
# `hal::bus` and a lib crate accepting a `&dyn Bus` would be misfiled. Drivers
# may name lib crates, since composing them creates no route to hardware.
#
# Note the limit of any dependency-graph check: raw MMIO in Rust needs no
# dependency at all, so this cannot stop a driver writing to 0x3FF44008. The
# lint that does that is `#![forbid(unsafe_code)]` in the crate itself.
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

meta = json.load(sys.stdin)
root = meta["workspace_root"].replace("\\", "/").rstrip("/")

def rel(pkg):
    path = os.path.dirname(pkg["manifest_path"]).replace("\\", "/")
    # Relative to the workspace, never the absolute path: matching "/lib/" as a
    # substring made every crate a "portable library" when the checkout itself
    # lived under a directory called lib.
    return path[len(root) + 1:] if path.startswith(root + "/") else path

def category(r):
    # Longest first, so drivers/physical wins over drivers.
    for d in sorted(TIERS, key=len, reverse=True):
        if r == d or r.startswith(d + "/"):
            return d
    return None

TIERS = {
    "arch":             "CPU-core crate",
    "soc":              "SoC crate",
    "drivers/physical": "Layer-1 physical driver",
    "drivers/bus":      "Layer-2 bus abstraction",
    "drivers/logical":  "Layer-3 logical driver",
    "lib":              "portable library",
}

# Membership is read from the workspace, so a new soc or lib crate is usable by
# its neighbours without editing this file.
def names_in(d):
    return {p["name"] for p in meta["packages"] if category(rel(p)) == d}

SOCS = names_in("soc")
LIBS = names_in("lib")

ALLOWED = {
    "arch":             {"hal"},
    "soc":              {"hal"},
    "drivers/physical": {"hal"} | SOCS,
    "drivers/bus":      {"api"} | LIBS,
    "drivers/logical":  {"api"} | LIBS,
    "lib":              LIBS,
}
DESCRIBE = {
    "arch":             "hal",
    "soc":              "hal",
    "drivers/physical": "hal and soc/ crates",
    "drivers/bus":      "api and lib/ crates",
    "drivers/logical":  "api and lib/ crates",
    "lib":              "other lib/ crates only",
}

violations = []
checked = 0

for pkg in meta["packages"]:
    r = rel(pkg)
    # `hal` is the root of the graph: it may name nothing.
    if pkg["name"] == "hal":
        tier, allowed, rule = "hal", set(), "nothing -- hal is the root of the graph"
    else:
        tier = category(r)
        if tier is None:
            continue
        allowed, rule = ALLOWED[tier], DESCRIBE[tier]
    checked += 1
    for dep in pkg["dependencies"]:
        # dev-dependencies are test scaffolding; they never ship in the image
        # and so cannot leak hardware access into a crate's public surface.
        if dep["kind"] == "dev":
            continue
        if dep["name"] not in allowed:
            label = TIERS.get(tier, "hal")
            violations.append(
                "LAYER VIOLATION: %s (%s) depends on %s -- may depend only on %s"
                % (pkg["name"], label, dep["name"], rule)
            )

for v in violations:
    print(v)

if violations:
    print("")
    print("%d layer violation(s) found." % len(violations))
    sys.exit(1)

if checked == 0:
    print("check-layers: matched no crates -- has the layout moved?")
    sys.exit(1)

print("Layer boundary OK: %d crates within their tier's whitelist "
      "(%d soc, %d lib)." % (checked, len(SOCS), len(LIBS)))
EOF
)

cargo metadata --format-version 1 --no-deps | "$PY" -c "$PYSRC"
