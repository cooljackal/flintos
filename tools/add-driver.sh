#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Create a NEW driver crate from a template and register it in the workspace.
#   make add-driver NAME=bh1750                     # logical (default)
#   make add-driver NAME=lcd CATEGORY=physical      # esp32 physical, from _template
#   make add-driver NAME=spi3-bus CATEGORY=bus
# Category picks the tier and the naming rule:
#   physical -> drivers/physical/<soc>/<name>, package <soc>-<name> (SOC=esp32)
#   bus      -> drivers/bus/<name>,            package <name>
#   logical  -> drivers/logical/<name>,        package <name>
# (enable it in an app afterwards with `make enable-driver`.)
set -eu

cd "$(dirname "$0")/.."

NAME="${NAME:-}"
CATEGORY="${CATEGORY:-logical}"
SOC="${SOC:-esp32}"
DESC="${DESC:-}"

if [ -z "$NAME" ]; then
	echo "usage: make add-driver NAME=<name> [CATEGORY=physical|bus|logical] [DESC=\"...\"]" >&2
	exit 1
fi
case "$NAME" in [a-z]*) ;; *)
	echo "error: NAME must start with a lowercase letter (got '$NAME')" >&2
	exit 1 ;;
esac
if printf '%s' "$NAME" | grep -q '[^a-z0-9-]'; then
	echo "error: NAME may contain only lowercase letters, digits and '-'" >&2
	exit 1
fi

case "$CATEGORY" in
physical)
	DIR="drivers/physical/$SOC/$NAME"
	PKG="$SOC-$NAME"
	;;
bus)
	DIR="drivers/bus/$NAME"
	PKG="$NAME"
	;;
logical)
	DIR="drivers/logical/$NAME"
	PKG="$NAME"
	;;
*)
	echo "error: CATEGORY must be physical, bus, or logical (got '$CATEGORY')" >&2
	exit 1
	;;
esac

if [ -e "$DIR" ]; then
	echo "error: $DIR already exists" >&2
	exit 1
fi
[ -n "$DESC" ] || DESC="$PKG driver"
IDENT=$(printf '%s' "$PKG" | tr '-' '_')

mkdir -p "$DIR/src"

if [ "$CATEGORY" = physical ]; then
	# Depth from drivers/physical/<soc>/<name> to the repo root is four levels.
	cat >"$DIR/Cargo.toml" <<EOF
# SPDX-License-Identifier: Apache-2.0

[package]
name = "$PKG"
publish = false
version.workspace = true
edition.workspace = true
description = "$DESC"
license.workspace = true
authors.workspace = true
repository.workspace = true
rust-version.workspace = true

# A Layer-1 physical driver may name only \`hal\` and the chip's \`soc\` crate --
# tools/check-layers.sh enforces exactly this whitelist.
[dependencies]
hal = { path = "../../../../hal" }
soc-$SOC = { path = "../../../../soc/$SOC" }
EOF
	# The template's lib.rs is the reference implementation of the conventions.
	sed -e "s/esp32_template/$IDENT/g" \
		-e "s/esp32-_template/$PKG/g" \
		drivers/physical/esp32/_template/src/lib.rs >"$DIR/src/lib.rs"
else
	# bus / logical: two levels above <name> is drivers/, three is the root.
	cat >"$DIR/Cargo.toml" <<EOF
# SPDX-License-Identifier: Apache-2.0

[package]
name = "$PKG"
publish = false
version.workspace = true
edition.workspace = true
description = "$DESC"
license.workspace = true
authors.workspace = true
repository.workspace = true
rust-version.workspace = true

[dependencies]
# Layer-2/3 drivers depend only on api (which re-exports hal::bus) -- see
# tools/check-layers.sh. Add a bus crate (i2c-bus / spi-bus) if this part
# talks over one.
api = { path = "../../../api" }
EOF
	cat >"$DIR/src/lib.rs" <<EOF
// SPDX-License-Identifier: Apache-2.0

//! $DESC
//!
//! A Layer-$([ "$CATEGORY" = bus ] && echo 2 || echo 3) ($CATEGORY) driver. See drivers/README.md for the
//! conventions this crate should follow.

#![no_std]
EOF
fi

# Register in the workspace members list (drivers are listed, not globbed),
# grouped after the last entry of the same tier.
python - "$DIR" <<'PY'
import re, sys
member = sys.argv[1].replace("\\", "/")
tier = "/".join(member.split("/")[:2]) + "/"   # e.g. drivers/physical/
text = open("Cargo.toml", encoding="utf-8").read()
lines = text.splitlines(keepends=True)
line = '    "%s",\n' % member
# Find the last existing member line under the same tier; insert after it.
last = None
for i, ln in enumerate(lines):
    if re.match(r'\s*"%s' % re.escape(tier), ln):
        last = i
if last is None:
    # No sibling tier yet: insert before the members-closing bracket.
    for i, ln in enumerate(lines):
        if ln.strip() == "]":
            last = i - 1
            break
lines.insert(last + 1, line)
open("Cargo.toml", "w", encoding="utf-8", newline="").write("".join(lines))
print("registered %s in workspace members" % member)
PY

echo "Created $DIR (package $PKG)"
echo ""
echo "Next:"
echo "  edit $DIR/src/lib.rs"
echo "  make enable-driver APP=<app> DRIVER=$NAME     # depend on it from an app"
echo "  make check-names && make check-layers          # confirm it fits the conventions"
