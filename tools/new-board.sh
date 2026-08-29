#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Scaffold a new board manifest by cloning an existing one, and do the wiring so
# the board is immediately selectable:
#   make new-board NAME=my-pico                     # clone the Pico (default)
#   make new-board NAME=my-wrover FROM=board-esp32-wrover
#   make new-board NAME=my-pico DESC="My Pico carrier"
#
# It creates board/src/<name>.rs (a clone of FROM's manifest), adds the
# `board-<name>` feature to `board` and `kernel`, registers the module in
# board/src/lib.rs (pub mod, pub use … as active, SELECTED count), and — for an
# RP2040 board — adds it to the Makefile's RP2040_BOARDS. The clone is identical
# to FROM; you then edit only what differs (typically a pin). See the
# "Add a board" tutorial.
set -eu

cd "$(dirname "$0")/.."

NAME="${NAME:-}"
FROM="${FROM:-board-raspberry-pi-pico}"
DESC="${DESC:-}"

if [ -z "$NAME" ]; then
	echo "usage: make new-board NAME=<name> [FROM=<board>] [DESC=\"...\"]" >&2
	exit 1
fi
# Feature names: lowercase, start with a letter, [a-z0-9-] thereafter. NAME is
# the bare name; the Cargo feature is `board-<name>`.
case "$NAME" in board-*)
	echo "error: give NAME without the 'board-' prefix (got '$NAME')" >&2
	exit 1 ;;
esac
case "$NAME" in [a-z]*) ;; *)
	echo "error: NAME must start with a lowercase letter (got '$NAME')" >&2
	exit 1 ;;
esac
if printf '%s' "$NAME" | grep -q '[^a-z0-9-]'; then
	echo "error: NAME may contain only lowercase letters, digits and '-'" >&2
	exit 1
fi

NEW_FEATURE="board-$NAME"
NEW_IDENT=$(printf '%s' "$NAME" | tr '-' '_')
FROM_IDENT=$(printf '%s' "${FROM#board-}" | tr '-' '_')
FROM_FILE="board/src/$FROM_IDENT.rs"
NEW_FILE="board/src/$NEW_IDENT.rs"

# The template must be a real board, and the new one must not already exist.
if [ ! -f "$FROM_FILE" ]; then
	echo "error: FROM board '$FROM' has no manifest ($FROM_FILE)" >&2
	exit 1
fi
if [ -e "$NEW_FILE" ] || grep -q "^$NEW_FEATURE " board/Cargo.toml; then
	echo "error: a board named '$NEW_FEATURE' already exists" >&2
	exit 1
fi
if ! grep -q "^$FROM = " board/Cargo.toml || ! grep -q "^$FROM = " kernel/Cargo.toml; then
	echo "error: FROM board '$FROM' is not registered in board/ and kernel/" >&2
	exit 1
fi

# The board's human name: DESC if given, else the template's name plus a note.
if [ -z "$DESC" ]; then
	FROM_NAME=$(sed -n 's/^pub const BOARD_NAME: &str = "\(.*\)";/\1/p' "$FROM_FILE")
	DESC="${FROM_NAME:-$NAME} (clone)"
fi

# ── 1. The manifest: a clone of FROM's, with our own header and name ──────────
# Body = from the first `use` line to just before the template's own test
# module (a clone's invariant tests are the template's; the generic manifest
# tests in lib.rs still run against this board).
{
	echo "// SPDX-License-Identifier: Apache-2.0"
	echo ""
	echo "//! $DESC."
	echo "//!"
	echo "//! Scaffolded from \`$FROM\` by \`make new-board\`."
	echo "//!"
	echo "//! Every fact is a clone of that board; edit only what differs here (a pin)."
	echo ""
	awk '
		# Once the body has started, copy it until the template test module.
		started { if ($0 ~ /^#\[cfg\(test\)\]/) exit; print; next }
		# Skip the leading license + `//!` doc header (comments and blanks).
		/^\/\// || /^[[:space:]]*$/ { next }
		# First real line of code: the body begins here.
		{ started = 1; print }
	' "$FROM_FILE"
} >"$NEW_FILE"
# Swap in this board's name (no regex, so the text is taken literally).
awk -v nm="$DESC" '
	/^pub const BOARD_NAME: &str = / {
		print "pub const BOARD_NAME: &str = \"" nm "\";"; next
	}
	{ print }
' "$NEW_FILE" >"$NEW_FILE.tmp" && mv "$NEW_FILE.tmp" "$NEW_FILE"

# ── 2. Feature in board/ and kernel/ (copied from FROM, name swapped) ─────────
# board/Cargo.toml: same right-hand side as FROM (same driver bundle).
FROM_RHS=$(sed -n "s/^$FROM = //p" board/Cargo.toml)
awk -v anchor="^$FROM = " -v line="$NEW_FEATURE = $FROM_RHS" '
	{ print }
	$0 ~ anchor && !done { print line; done = 1 }
' board/Cargo.toml >board/Cargo.toml.tmp && mv board/Cargo.toml.tmp board/Cargo.toml

# kernel/Cargo.toml: FROM's whole line with the board name swapped (this also
# rewrites the `board/board-<from>` mapping to `board/<new-feature>`).
KERNEL_LINE=$(sed -n "s/^$FROM = .*/&/p" kernel/Cargo.toml | sed "s/$FROM/$NEW_FEATURE/g")
awk -v anchor="^$FROM = " -v line="$KERNEL_LINE" '
	{ print }
	$0 ~ anchor && !done { print line; done = 1 }
' kernel/Cargo.toml >kernel/Cargo.toml.tmp && mv kernel/Cargo.toml.tmp kernel/Cargo.toml

# ── 3. Register the module in board/src/lib.rs ────────────────────────────────
LIB=board/src/lib.rs
# pub mod, after FROM's module declaration.
awk -v pat="pub mod $FROM_IDENT;" -v feat="$NEW_FEATURE" -v ident="$NEW_IDENT" '
	{ print }
	index($0, pat) && !done {
		print ""
		print "#[cfg(feature = \"" feat "\")]"
		print "pub mod " ident ";"
		done = 1
	}
' "$LIB" >"$LIB.tmp" && mv "$LIB.tmp" "$LIB"
# pub use … as active, after FROM's arm.
awk -v pat="pub use $FROM_IDENT as active;" -v feat="$NEW_FEATURE" -v ident="$NEW_IDENT" '
	{ print }
	index($0, pat) && !done {
		print ""
		print "#[cfg(feature = \"" feat "\")]"
		print "pub use " ident " as active;"
		done = 1
	}
' "$LIB" >"$LIB.tmp" && mv "$LIB.tmp" "$LIB"
# SELECTED count, on the line above the `new-board:selected` anchor.
awk -v feat="$NEW_FEATURE" '
	index($0, "new-board:selected") && !done {
		print "    + cfg!(feature = \"" feat "\") as usize"
		done = 1
	}
	{ print }
' "$LIB" >"$LIB.tmp" && mv "$LIB.tmp" "$LIB"

# ── 3b. Make any shared base module the clone re-exports reachable ────────────
# A template may re-export a sibling base module (`pub use crate::<base>::…`)
# that is gated on a fixed list of board features, e.g. the ESP32 WROOM base.
# The clone re-exports it too, so its feature must appear in that gate. Bases
# already keyed on a driver bundle (the RP2040 base) need nothing.
BASES=$(grep -oE 'pub use (crate|super)::[a-z0-9_]+::' "$NEW_FILE" \
	| sed -E 's/^pub use (crate|super):://; s/::$//' | sort -u)
for base in $BASES; do
	awk -v base="$base" -v feat="$NEW_FEATURE" '
		{ line[NR] = $0 }
		END {
			for (i = 1; i <= NR; i++) {
				c = line[i]
				if ((c == "mod " base ";" || c == "pub mod " base ";") && i > 1) {
					p = line[i - 1]
					if (p ~ /any\(/ && p ~ /feature = "board-/ \
					    && index(p, "\"" feat "\"") == 0) {
						sub(/\)\)\]/, ", feature = \"" feat "\"))]", line[i - 1])
					}
				}
			}
			for (i = 1; i <= NR; i++) print line[i]
		}
	' "$LIB" >"$LIB.tmp" && mv "$LIB.tmp" "$LIB"
done

# ── 4. Teach `make` it is an ARM board, if the template is one ────────────────
IS_ARM=no
if grep '^RP2040_BOARDS' Makefile | grep -q "$FROM"; then
	IS_ARM=yes
	sed "/^RP2040_BOARDS/ s/\$/ $NEW_FEATURE/" Makefile >Makefile.tmp \
		&& mv Makefile.tmp Makefile
fi

echo "Created $NEW_FILE and registered $NEW_FEATURE (cloned from $FROM)."
echo ""
echo "Next:"
echo "  edit $NEW_FILE            # change the facts that differ (e.g. USER_LED)"
if [ "$IS_ARM" = yes ]; then
	echo "  make flash APP=blinky BOARD=$NEW_FEATURE   # build, flash, monitor"
else
	echo "  make flash APP=blinky BOARD=$NEW_FEATURE PORT=<port>"
fi
echo "  cargo test -p board --no-default-features --features $NEW_FEATURE"
