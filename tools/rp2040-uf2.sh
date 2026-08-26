#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Convert an RP2040 ELF into a raw .bin and a .uf2, cross-platform. Replaces the
# convert action of rp2040-image.ps1 -- it is just objcopy + elf2uf2-rs, the same
# two tools that script drove, so no PowerShell is needed.
#
# Usage: rp2040-uf2.sh <elf> <bin> <uf2>
#   FLINT_OBJCOPY   objcopy to use (default: arm-none-eabi-objcopy, then llvm-objcopy)
#   FLINT_ELF2UF2   elf2uf2 to use (default: elf2uf2-rs)

set -euo pipefail

ELF="${1:?usage: $0 <elf> <bin> <uf2>}"
BIN="${2:?usage: $0 <elf> <bin> <uf2>}"
UF2="${3:?usage: $0 <elf> <bin> <uf2>}"

pick() {  # first of the candidates that exists on PATH
    local c
    for c in "$@"; do command -v "$c" >/dev/null 2>&1 && { echo "$c"; return 0; }; done
    return 1
}

OBJCOPY="${FLINT_OBJCOPY:-$(pick arm-none-eabi-objcopy llvm-objcopy objcopy || true)}"
ELF2UF2="${FLINT_ELF2UF2:-$(pick elf2uf2-rs || true)}"
[ -n "$OBJCOPY" ] || { echo "no objcopy found (set FLINT_OBJCOPY)" >&2; exit 1; }
[ -n "$ELF2UF2" ] || { echo "elf2uf2-rs not found (cargo install elf2uf2-rs, or set FLINT_ELF2UF2)" >&2; exit 1; }

"$OBJCOPY" -O binary "$ELF" "$BIN"
# elf2uf2-rs reads the ELF's load addresses and stamps the RP2040 family id.
"$ELF2UF2" "$ELF" "$UF2"
echo "built $UF2"
