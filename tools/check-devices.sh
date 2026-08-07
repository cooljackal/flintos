#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Which drivers keep which device-class promises.
#
# A `lib/` crate defines a contract -- LedStrip, Dimmable, Display -- and any
# number of drivers implement it. Rust makes that partial by design: a driver
# implements what its hardware does and stays silent about the rest, which is
# correct, because a chip with no brightness register should not pretend.
#
# The problem is that silence has two meanings:
#
#   "this chip cannot do it"      and      "nobody has got round to it"
#
# Both look identical -- a missing `impl`. This prints the table so the second
# one is visible as a blank rather than absent, the way a missing row in a
# status table is visible.
#
# Reports, never fails. A gap is information, not an error: `make lint` decides
# what breaks the build, and "this chip has no brightness register" must not.
#
# Reads the source rather than the type system, because `impl Trait for Type`
# is not in `cargo metadata` and the alternative is a compiler plugin. That
# makes it a good-faith index, not a proof -- a trait implemented through a
# macro or a blanket impl will not show up here.

set -euo pipefail
cd "$(dirname "$0")/.."

PY=""
for candidate in python3 python py; do
    if command -v "$candidate" >/dev/null 2>&1 &&
       "$candidate" -c 'import re,sys' >/dev/null 2>&1; then
        PY="$candidate"
        break
    fi
done
[ -n "$PY" ] || {
    echo "check-devices: needs a working python3 (or python)" >&2
    exit 1
}

"$PY" - <<'EOF'
import os, re, sys

ROOT = os.getcwd()

def crates(*dirs):
    """(crate name, its src directory) for every crate under `dirs`."""
    out = []
    for d in dirs:
        base = os.path.join(ROOT, *d.split("/"))
        if not os.path.isdir(base):
            continue
        for name in sorted(os.listdir(base)):
            src = os.path.join(base, name, "src")
            if os.path.isdir(src):
                out.append((name, src))
    return out

def read(src):
    text = []
    for dirpath, _, files in os.walk(src):
        for f in files:
            if f.endswith(".rs"):
                with open(os.path.join(dirpath, f), encoding="utf-8", errors="replace") as fh:
                    text.append(fh.read())
    return "\n".join(text)

# Contracts are `pub trait` in a lib/ crate. Anything a driver could implement.
contracts = {}          # trait name -> defining crate
for name, src in crates("lib"):
    for m in re.finditer(r"^\s*pub trait\s+([A-Za-z_][A-Za-z0-9_]*)", read(src), re.M):
        contracts[m.group(1)] = name

if not contracts:
    print("No device-class contracts found in lib/ -- nothing to report yet.")
    sys.exit(0)

# Who implements what. Strip test modules first: a mock in a #[cfg(test)] block
# is not a driver keeping a promise, and counting it would report coverage that
# does not ship.
impls = {t: [] for t in contracts}
for name, src in crates("drivers/logical", "drivers/physical", "drivers/bus"):
    body = read(src)
    body = re.sub(r"#\[cfg\(test\)\]\s*mod\s+\w+\s*\{.*", "", body, flags=re.S)
    for trait in contracts:
        if re.search(r"^\s*impl(?:<[^>]*>)?\s+%s(?:<[^>]*>)?\s+for\s" % re.escape(trait), body, re.M):
            impls[trait].append(name)

width = max(len(t) for t in contracts)
print("Device-class coverage\n")
print("  %-*s  %-12s  %s" % (width, "CONTRACT", "FROM", "IMPLEMENTED BY"))
print("  %s  %s  %s" % ("-" * width, "-" * 12, "-" * 40))

gaps = 0
for trait in sorted(contracts):
    who = ", ".join(sorted(impls[trait])) if impls[trait] else "(nobody yet)"
    if not impls[trait]:
        gaps += 1
    print("  %-*s  %-12s  %s" % (width, trait, contracts[trait], who))

print("")
print("%d contract(s), %d with no implementor." % (len(contracts), gaps))
print("A blank is not a failure -- a chip that cannot do a thing should not")
print("claim it. It is here so the gap is visible rather than merely absent.")
EOF
