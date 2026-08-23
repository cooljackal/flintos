#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Application-hygiene guard for apps/examples/ (issue #122, plan "The rule
# afterwards").
#
# check-layers.sh proves an example names no soc/ or drivers/physical crate in
# its manifest. This is the other half: the source itself must not reach past
# the board to touch a chip directly. A dependency-graph check cannot see that
# -- `use soc_esp32::...` needs the crate in Cargo.toml, which check-layers
# catches, but a `static mut` register cell or an `addr_of!` into MMIO needs no
# dependency at all. So this greps the source.
#
# What it bans under apps/examples/, and why each is gone from a clean app:
#
#   static mut     A driver the board handed back is a `&'static`, shared
#                  through `api`. An app that still keeps its own `static mut`
#                  cell is holding a peripheral the old way.
#   addr_of!       Its only use in an app was to take a reference to that
#                  `static mut` without tripping the lint. No cell, no addr_of.
#   soc_esp32::    Naming a soc path in source is an app reaching for the pin
#   soc_rp2040::   matrix or a clock gate itself, which is the board's job.
#
# NOT banned yet: `unsafe`. blink still runs an RMT feed from an ISR and pokes
# dport over FFI, and pwm drives LEDC directly; both keep `unsafe` until the
# LEDC/RMT `on_pin` follow-up lets them open those peripherals through the
# board. Ban `unsafe` outright once that lands -- it is the last reach-through.
#
# apps/tests/ is exempt entirely: a probe's whole job is to exercise the
# machinery, so it names soc crates and writes the unsafe the app rule forbids.
# This script only ever looks under apps/examples/.
#
# Exit non-zero on any hit. Wired into CI and runnable by hand.

set -euo pipefail
cd "$(dirname "$0")/.."

DIR="apps/examples"

# Each entry: a human name, then an extended-regex. `static mut` is anchored to
# the start of a line (after optional `pub`) so the phrase in a "no static mut"
# comment is not a hit; the others cannot plausibly appear in prose.
patterns=(
    "static mut declaration|^[[:space:]]*(pub[[:space:]]+)?static[[:space:]]+mut[[:space:]]"
    "addr_of / addr_of_mut|\\baddr_of(_mut)?\\b"
    "soc-crate path in source|\\bsoc_(esp32|rp2040)::"
)

status=0
for entry in "${patterns[@]}"; do
    name=${entry%%|*}
    regex=${entry#*|}
    # --include limits the walk to Rust source; grep -rn gives file:line:text.
    if hits=$(grep -rnE --include='*.rs' "$regex" "$DIR"); then
        echo "::error::banned pattern under $DIR/: $name"
        echo "$hits" | sed 's/^/  /'
        status=1
    fi
done

if [ "$status" -ne 0 ]; then
    echo ""
    echo "An example may not reach past the board to a chip. See tools/check-apps.sh."
    exit 1
fi

echo "apps/examples clean: no static mut, addr_of, or soc-crate path in source."
