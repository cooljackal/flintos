#!/usr/bin/env bash
# Layer-boundary enforcement (plan W7.1).
#
# Layer-3 (drivers/logical/*) and Layer-2 (drivers/bus/*) crates may depend ONLY
# on flint-api. Depending on flint-hal or any flint-arch-* crate is a layer
# violation: it would give a device/bus driver access to hardware register
# definitions, defeating the three-layer model. This is the structural boundary
# the design promises — enforce it here so it cannot silently rot.
#
# Exit non-zero on any violation. Wire into CI and `make check-layers`.

set -euo pipefail
cd "$(dirname "$0")/.."

violations=0
for manifest in drivers/logical/*/Cargo.toml drivers/bus/*/Cargo.toml; do
    [ -e "$manifest" ] || continue
    crate_dir=$(dirname "$manifest")
    # Look only inside [dependencies]-style lines for forbidden crates.
    if grep -Eq '^\s*flint-hal\s*=' "$manifest"; then
        echo "LAYER VIOLATION: $crate_dir depends on flint-hal (must use flint-api)"
        violations=$((violations + 1))
    fi
    if grep -Eq '^\s*flint-arch' "$manifest"; then
        echo "LAYER VIOLATION: $crate_dir depends on a flint-arch-* crate (Layer-1 only)"
        violations=$((violations + 1))
    fi
done

if [ "$violations" -ne 0 ]; then
    echo ""
    echo "$violations layer violation(s) found. Logical/bus drivers must depend only on flint-api."
    exit 1
fi
echo "Layer boundary OK: all logical/bus drivers depend only on flint-api."
