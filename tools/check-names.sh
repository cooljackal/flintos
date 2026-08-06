#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Package naming and layout enforcement.
#
# The convention is stated in the root Cargo.toml; this is what makes it true.
# It went unchecked until two crates -- `bme280` and `ssd1306` -- had shipped
# under names already owned by well-known third-party crates on crates.io. A
# convention that lives only in a comment is a convention that drifts, and the
# repo already has the pattern for fixing that: tools/check-layers.sh.
#
# Three rules:
#
#   1. A publishable package is named `flint-<slug>`. A package may drop the
#      prefix only by setting `publish = false` -- the applications do, because
#      the package name is also `make APP=<name>` and the flashed ELF filename.
#
#   2. A package's directory leaf is its slug, or its slug with a leading
#      category word removed when the slug repeats one (`flint-arch-xtensa` ->
#      `arch/xtensa/`, `flint-driver-bme280` -> `drivers/logical/bme280/`).
#
#   3. A Layer-3 logical driver is named `flint-driver-<device>`, the namespace
#      the community driver convention reserves. A third-party BME280 driver is
#      `flint-driver-bme280` too, so a first-party one cannot sit elsewhere
#      without splitting the namespace in two.
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

PREFIX = "flint-"
DRIVER_PREFIX = "flint-driver-"
LOGICAL_DIR = "drivers/logical"

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

    # cargo metadata reports `publish = false` as an empty allow-list, and a
    # publishable package as null. Anything else is an explicit registry list,
    # which still means publishable.
    publishable = pkg.get("publish") != []

    # Rule 1 -- prefix, unless the package opts out of publishing.
    if not name.startswith(PREFIX):
        if publishable:
            violations.append(
                "NAME: %s has no `flint-` prefix and is publishable\n"
                "      fix: rename to %s%s, or add `publish = false` if it is "
                "never published" % (name, PREFIX, name)
            )
        slug = name
    else:
        slug = name[len(PREFIX):]

    # Rule 2 -- directory leaf is the slug, or the slug minus a leading category
    # word that repeats an ancestor directory (tolerating drivers/ -> driver-).
    if leaf != slug:
        ok = False
        if "-" in slug:
            word, rest = slug.split("-", 1)
            if rest == leaf and any(
                a == word or a == word + "s" for a in ancestors
            ):
                ok = True
        if not ok:
            violations.append(
                "LAYOUT: %s lives in %s/ -- expected leaf `%s`, or `<category>-%s` "
                "with the category naming an ancestor directory"
                % (name, rel, slug, leaf)
            )

    # Rule 3 -- Layer-3 drivers claim the reserved community namespace.
    if ("/" + LOGICAL_DIR + "/") in "/" + rel + "/" and not name.startswith(DRIVER_PREFIX):
        violations.append(
            "NAMESPACE: %s is a Layer-3 logical driver and must be named `%s<device>`\n"
            "           (see \"Community Driver Convention\" in the plan)"
            % (name, DRIVER_PREFIX)
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
