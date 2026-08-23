#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Scaffold a new application under apps/examples/ from the `hello` template.
#   make new-app NAME=blinky
#   make new-app NAME=blinky DESC="Blinks the onboard LED"
# The workspace `members` globs apps/examples/*, so no Cargo.toml edit is needed;
# `make flash APP=<name> BOARD=<board>` works immediately.
set -eu

cd "$(dirname "$0")/.."

NAME="${NAME:-}"
DESC="${DESC:-}"

if [ -z "$NAME" ]; then
	echo "usage: make new-app NAME=<name> [DESC=\"...\"]" >&2
	exit 1
fi
# Cargo package names: lowercase, start with a letter, [a-z0-9-] thereafter.
case "$NAME" in
[a-z]*) ;;
*)
	echo "error: NAME must start with a lowercase letter (got '$NAME')" >&2
	exit 1
	;;
esac
if printf '%s' "$NAME" | grep -q '[^a-z0-9-]'; then
	echo "error: NAME may contain only lowercase letters, digits and '-' (got '$NAME')" >&2
	exit 1
fi

DEST="apps/examples/$NAME"
if [ -e "$DEST" ] || [ -e "apps/tests/$NAME" ]; then
	echo "error: an app named '$NAME' already exists" >&2
	exit 1
fi

# Rust identifiers can't contain '-'; the task fn uses the underscored form,
# the task-name string keeps the crate name.
IDENT=$(printf '%s' "$NAME" | tr '-' '_')
[ -n "$DESC" ] || DESC="FlintOS application: $NAME"

mkdir -p "$DEST/src"

# build.rs is identical for every app.
cp apps/examples/hello/build.rs "$DEST/build.rs"

# Cargo.toml: hello's, with the name and description swapped.
sed -e "s/^name = \"hello\"/name = \"$NAME\"/" \
	-e "s/^description = \".*\"/description = \"$DESC\"/" \
	apps/examples/hello/Cargo.toml >"$DEST/Cargo.toml"

# main.rs: a minimal one task, tagless logging on a timer.
cat >"$DEST/src/main.rs" <<EOF
// SPDX-License-Identifier: Apache-2.0

//! $DESC

#![no_std]
#![no_main]

use api::prelude::*;

kernel::flint_app!(main, abi = 2);

fn main() {
    Task::new("$NAME", $IDENT).spawn().expect("spawn");
}

fn $IDENT() {
    let mut n = 0u32;
    loop {
        n += 1;
        log_info!("n={n}");
        sleep_ms(1000);
    }
}
EOF

echo "Created $DEST"
echo ""
echo "Next:"
echo "  make flash APP=$NAME BOARD=<board>       # build, flash, monitor"
echo "  make enable-driver APP=$NAME DRIVER=<x>  # add a hardware driver"
echo "  make drivers                             # see what drivers exist"
