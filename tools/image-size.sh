#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Report where a Flint image's bytes went, per memory region.
#
# Section sizes on their own are not actionable: an ESP32 image is scattered
# across four regions with wildly different budgets -- 127 KiB of IRAM, 64 KiB
# of DRAM, megabytes of flash -- and "your binary is 120 KiB" says nothing about
# whether the next feature will fit. What matters is how full each region is,
# and which one runs out first. That is almost always IRAM or DRAM, long before
# flash.
#
# Region sizes are parsed out of the linker script rather than duplicated here,
# so this cannot drift from the map the image was actually linked against.
#
# Usage: tools/image-size.sh <elf> [linker-script]

set -eu

ELF=${1:?usage: image-size.sh <elf> [linker-script]}
LD=${2:-arch/flint-arch-xtensa/flint32.ld}

SIZE=${XTENSA_SIZE:-xtensa-esp32-elf-size}

# The Makefile puts the toolchain on PATH, but on Windows it does so as a
# native path, which this shell cannot use. Fall back to the standard espup
# install locations before giving up.
if ! command -v "$SIZE" >/dev/null 2>&1; then
    home_win=""
    if [ -n "${USERPROFILE:-}" ]; then
        home_win=$(cygpath -u "$USERPROFILE" 2>/dev/null || printf '%s' "$USERPROFILE")
    fi
    for dir in \
        "${HOME:-}/.rustup/toolchains/esp/xtensa-esp-elf/bin" \
        "$home_win/.rustup/toolchains/esp/xtensa-esp-elf/bin"
    do
        [ -n "$dir" ] || continue
        for ext in "" ".exe"; do
            if [ -x "$dir/xtensa-esp32-elf-size$ext" ]; then
                SIZE="$dir/xtensa-esp32-elf-size$ext"
                break 2
            fi
        done
    done
fi

if ! command -v "$SIZE" >/dev/null 2>&1; then
    echo "image-size: xtensa-esp32-elf-size not found; skipping size report" >&2
    exit 0
fi

if [ ! -f "$ELF" ]; then
    echo "image-size: $ELF not found" >&2
    exit 1
fi

# App partition capacity. espflash's default 4 MB layout puts the factory app
# at 0x10000 with 0x3F0000 available; override if you use your own table.
APP_PARTITION_BYTES=${APP_PARTITION_BYTES:-$((0x3F0000))}

"$SIZE" -A "$ELF" | awk -v ld="$LD" -v part="$APP_PARTITION_BYTES" '
function human(n) {
    if (n >= 1048576) return sprintf("%.1f MiB", n / 1048576)
    if (n >= 1024)    return sprintf("%.1f KiB", n / 1024)
    return sprintf("%d B", n)
}
function bar(pct,   i, s, filled) {
    filled = int(pct / 5 + 0.5); if (filled > 20) filled = 20
    s = ""
    for (i = 0; i < 20; i++) s = s (i < filled ? "#" : ".")
    return s
}
BEGIN {
    # Region lengths from the linker script MEMORY block.
    while ((getline line < ld) > 0) {
        if (match(line, /^[ \t]*([A-Za-z0-9_]+)[ \t]*\([RWX]+\)[ \t]*:[ \t]*ORIGIN[ \t]*=[ \t]*(0x[0-9A-Fa-f]+)[ \t]*,[ \t]*LENGTH[ \t]*=[ \t]*(0x[0-9A-Fa-f]+)/, m)) {
            capacity[m[1]] = strtonum(m[3])
            origin[m[1]] = strtonum(m[2])
        }
    }
    close(ld)

    # Which linker region an address belongs to. Grouped for reporting: the
    # vectors share the IRAM budget, and both flash windows share the partition.
    nregions = 0
    split("vectors_seg iram_seg dram_seg task_stacks panic_region dma_pool drom_seg irom_seg", order, " ")
}
# `size -A` emits "<section> <size> <addr>" for allocated sections only.
$1 ~ /^\./ && NF >= 3 {
    sz = $2 + 0; addr = $3 + 0
    if (sz == 0) next
    for (r in origin) {
        if (addr >= origin[r] && addr < origin[r] + capacity[r]) {
            used[r] += sz
            break
        }
    }
    total += sz
}
END {
    printf "\n  Image: %s\n\n", "'"$ELF"'"
    printf "  %-14s %10s %10s  %-20s %6s\n", "REGION", "USED", "SIZE", "", "FULL"
    flash_used = 0; flash_cap = 0
    for (i = 1; i <= 8; i++) {
        r = order[i]
        if (!(r in capacity)) continue
        u = (r in used) ? used[r] : 0
        if (r == "drom_seg" || r == "irom_seg") { flash_used += u; flash_cap += capacity[r] }
        if (u == 0) continue
        pct = 100.0 * u / capacity[r]
        printf "  %-14s %10s %10s  %-20s %5.1f%%\n", r, human(u), human(capacity[r]), bar(pct), pct
    }
    if (flash_used > 0) {
        pct = 100.0 * flash_used / part
        printf "\n  %-14s %10s %10s  %-20s %5.1f%%\n", "flash image", human(flash_used), human(part), bar(pct), pct
        printf "  %-14s %s\n", "", "(default 4 MB espflash layout: factory app at 0x10000)"
    }
    printf "\n"
}
'
