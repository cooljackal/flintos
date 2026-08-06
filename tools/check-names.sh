#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Package naming and layout enforcement.
#
# The convention is stated in the root Cargo.toml; this is what makes it true.
# A convention that lives only in a comment is a convention that drifts, and the
# repo already has the pattern for fixing that: tools/check-layers.sh.
#
# Nothing in this workspace is published. Every package is a path member of one
# repo, so a package name only ever has to be unique here -- and short, plain
# names (`hal`, `api`, `kernel`) read better in every `use` and every `-p` flag
# than a repo prefix repeated twenty times. That only stays safe while the
# packages stay unpublished: `hal` and `api` are names this project has no claim
# to on crates.io, so a stray `cargo publish` would be both a mistake and,
# for most of them, a collision. `publish = false` is what forecloses it, which
# makes it the invariant worth checking rather than the name shape it permits.
#
# Three rules:
#
#   1. Every package sets `publish = false`. Cargo reports that as an empty
#      allow-list in `cargo metadata`; a publishable package is null, and an
#      explicit registry list still means publishable.
#
#   2. No package name carries a `flint-` prefix -- the inverse of the rule this
#      file used to enforce, kept so the old convention cannot creep back in one
#      crate at a time.
#
#   3. A package's directory leaf is its name, or its name with a leading
#      category word removed when the name repeats one (`arch-xtensa` ->
#      `arch/xtensa/`, `soc-esp32` -> `soc/esp32/`, `build` -> `tools/build/`).
#
# Exit non-zero on any violation. Wired into CI and `make check-names`.

set -euo pipefail
cd "$(dirname "$0")/.."

# See tools/check-layers.sh: Windows ships a non-interpreter `python3` shim.
PY=""
for candidate in python3 python py; do
    if command -v "$candidate" >/dev/null 2>&1 &&
       "$candidate" -c 'import json,sys' >/dev/null 2>&1; then
        PY="$candidate"
        break
    fi
done
[ -n "$PY" ] || {
    echo "check-names: needs a working python3 (or python) to read cargo metadata" >&2
    exit 1
}

PYSRC=$(cat <<'EOF'
import json, os, sys

FORBIDDEN_PREFIX = "flint-"

meta = json.load(sys.stdin)
root = meta["workspace_root"].replace("\\", "/").rstrip("/") + "/"
violations = []

for pkg in sorted(meta["packages"], key=lambda p: p["name"]):
    name = pkg["name"]
    path = os.path.dirname(pkg["manifest_path"]).replace("\\", "/")
    rel = path[len(root):] if path.startswith(root) else path
    parts = rel.strip("/").split("/")
    leaf = parts[-1]
    ancestors = parts[:-1]

    # Rule 1 -- never published. cargo metadata reports `publish = false` as an
    # empty allow-list, and a publishable package as null. Anything else is an
    # explicit registry list, which still means publishable.
    if pkg.get("publish") != []:
        violations.append(
            "PUBLISH: %s is publishable\n"
            "         fix: add `publish = false` to %s/Cargo.toml -- no package "
            "in this workspace is published" % (name, rel)
        )

    # Rule 2 -- no repo prefix, now that the names need not be globally unique.
    if name.startswith(FORBIDDEN_PREFIX):
        violations.append(
            "NAME: %s carries the obsolete `%s` prefix\n"
            "      fix: rename to %s"
            % (name, FORBIDDEN_PREFIX, name[len(FORBIDDEN_PREFIX):])
        )

    # Rule 3 -- directory leaf is the name, or the name minus a leading category
    # word that repeats an ancestor directory (tolerating a plural ancestor, so
    # `drivers/` matches a `driver-` category word).
    if leaf != name:
        ok = False
        if "-" in name:
            word, rest = name.split("-", 1)
            if rest == leaf and any(
                a == word or a == word + "s" for a in ancestors
            ):
                ok = True
        if not ok:
            violations.append(
                "LAYOUT: %s lives in %s/ -- expected leaf `%s`, or `<category>-%s` "
                "with the category naming an ancestor directory"
                % (name, rel, name, leaf)
            )

for v in violations:
    print(v)

if violations:
    print("")
    print("%d naming violation(s) found. The convention is stated at the top of "
          "the root Cargo.toml." % len(violations))
    sys.exit(1)

print("Package naming OK: %d packages." % len(meta["packages"]))
EOF
)

cargo metadata --format-version 1 --no-deps | "$PY" -c "$PYSRC"
