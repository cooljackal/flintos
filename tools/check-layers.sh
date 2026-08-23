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
#   board                hal, api, soc/*,   the pin map; constructs drivers,
#                        physical, bus,     never names kernel
#                        lib/*
#   apps/examples/*      hal, api, board,   an application: composes ready
#                        kernel, bus,       drivers, never opens a peripheral
#                        logical, lib/*     itself
#   radio/*              hal, api, kernel,  the vendor blobs and their adapter
#                        soc/*, lib/*
#
# arch and soc hold what is specific to a CPU core or a chip *and shared*.
# Anything that is one peripheral is a driver, wherever it happens to sit
# today: RMT, the watchdogs and the RNG were modules of soc-esp32 and are
# drivers now. The test is whether a second peripheral driver would want it --
# an address map and a pin router yes, a pulse generator no.
#
# `radio/*` is the exception that proves the rule, and it is deliberately the
# only one. Espressif's Wi-Fi and BLE blobs are precompiled against FreeRTOS's
# object model and call out through a table of 121 function pointers -- tasks,
# queues, semaphores, event groups, malloc. Satisfying that means naming
# `kernel`, which nothing else in the tree may do.
#
# Confining it to one tier is what keeps the concession honest: the blob's
# demands stop at this boundary instead of leaking into the drivers. If a
# second crate ever wants `kernel`, that is the signal to ask why rather than
# to widen this list.
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
    "radio":            "radio adapter",
    "soc":              "SoC crate",
    "drivers/physical": "Layer-1 physical driver",
    "drivers/bus":      "Layer-2 bus abstraction",
    "drivers/logical":  "Layer-3 logical driver",
    "lib":              "portable library",
    "board":            "board manifest",
    # Only examples are pinned to the app rule. apps/tests/* is deliberately
    # absent: a probe's job is to exercise the machinery, so it names whatever
    # soc and physical crate it is testing. An absent prefix matches no tier and
    # is skipped, which is exactly the exemption we want.
    "apps/examples":    "example application",
}

# Membership is read from the workspace, so a new soc or lib crate is usable by
# its neighbours without editing this file.
def names_in(d):
    return {p["name"] for p in meta["packages"] if category(rel(p)) == d}

SOCS = names_in("soc")
LIBS = names_in("lib")
PHYSICAL = names_in("drivers/physical")
BUS = names_in("drivers/bus")
LOGICAL = names_in("drivers/logical")

ALLOWED = {
    "arch":             {"hal"},
    "soc":              {"hal"},
    "drivers/physical": {"hal"} | SOCS,
    "drivers/bus":      {"api"} | LIBS,
    "drivers/logical":  {"api"} | LIBS,
    "lib":              LIBS,
    "radio":            {"hal", "api", "kernel"} | SOCS | LIBS,
    # The board crate constructs drivers -- it opens a controller and wraps it
    # in a bus -- so it may name the physical and bus drivers, plus hal, api,
    # the soc crates and the libs. It may NOT name `kernel`: a board is a pin
    # map, not the scheduler that runs on it.
    "board":            {"hal", "api"} | SOCS | PHYSICAL | BUS | LIBS,
    # An application composes drivers the board already opened; it never opens a
    # peripheral itself. So it may name api and board (the ready devices),
    # kernel (for `flint_app!`), the Layer-2 bus and Layer-3 logical drivers,
    # and the libs -- but never a soc/ or drivers/physical/ crate, the mark of
    # an app reaching past the board to touch registers. hal is contracts only
    # (bus traits, config types) and carries no route to hardware, so it is
    # allowed as it is everywhere else.
    "apps/examples":    {"hal", "api", "board", "kernel"} | BUS | LOGICAL | LIBS,
}
DESCRIBE = {
    "arch":             "hal",
    "soc":              "hal",
    "drivers/physical": "hal and soc/ crates",
    "drivers/bus":      "api and lib/ crates",
    "drivers/logical":  "api and lib/ crates",
    "lib":              "other lib/ crates only",
    "radio":            "hal, api, kernel, soc/ and lib/ crates",
    "board":            "hal, api, soc/, drivers/physical, drivers/bus and lib/ crates",
    "apps/examples":    "hal, api, board, kernel, drivers/bus, drivers/logical and lib/ crates",
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
        # dev- and build-dependencies never ship in the image: dev-deps are test
        # scaffolding, and a build-dependency (every app's `build` = tools/build,
        # which emits the linker script) runs on the host at build time. Neither
        # can leak hardware access into a crate's public surface, so neither is a
        # layer concern.
        if dep["kind"] in ("dev", "build"):
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
