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
# Usage: tools/image-size.sh <elf> [linker-script] [size-tool]

set -eu

ELF=${1:?usage: image-size.sh <elf> [linker-script] [size-tool]}
LD=${2:-arch/xtensa/flint32.ld}
# The Makefile passes the tool path here as an argument rather than in the
# environment: on Windows, make may run recipes through WSL bash, and a variable
# exported from make does not survive that hop. An argument does.
SIZE_ARG=${3:-}

# Finding the toolchain is more work than it should be, because this script can
# be run by three different shells on Windows -- Git Bash, MSYS make's shell,
# and WSL bash -- and they disagree about what a path even looks like. Git Bash
# reads `C:/Users/x`, WSL reads `/mnt/c/Users/x`, neither reads `C:\Users\x`.
# So: collect every plausible spelling and try them all.
#
# The alternative is a report that silently does not appear on someone's
# machine, which is worse than a few lines of path juggling.

# Emit POSIX spellings of a Windows path, one per line.
posix_forms() {
    win=$1
    [ -n "$win" ] || return 0
    cygpath -u "$win" 2>/dev/null
    # C:\Users\x -> /c/Users/x (Git Bash) and /mnt/c/Users/x (WSL)
    slashed=$(printf '%s' "$win" | tr '\\' '/')
    case $slashed in
        [A-Za-z]:/*)
            drive=$(printf '%s' "$slashed" | cut -c1 | tr 'A-Z' 'a-z')
            rest=${slashed#?:}
            printf '/%s%s\n' "$drive" "$rest"
            printf '/mnt/%s%s\n' "$drive" "$rest"
            ;;
    esac
}

TOOL=xtensa-esp32-elf-size
SUBPATH=.rustup/toolchains/esp/xtensa-esp-elf/bin

# Every place the tool might be, most specific first. XTENSA_SIZE comes from the
# Makefile and is a native Windows path there, so it gets translated too --
# under WSL that is the difference between finding the tool and not.
candidates=$(
    {
        printf '%s\n' "${XTENSA_SIZE:-}"
        posix_forms "${XTENSA_SIZE:-}"
        printf '%s\n' "$TOOL"
        for home in "${HOME:-}" $(posix_forms "${USERPROFILE:-}"); do
            [ -n "$home" ] || continue
            printf '%s/%s/%s\n' "$home" "$SUBPATH" "$TOOL"
            printf '%s/%s/%s.exe\n' "$home" "$SUBPATH" "$TOOL"
        done
    } | grep -v '^$'
)

SIZE=""
for candidate in $candidates; do
    if command -v "$candidate" >/dev/null 2>&1; then
        SIZE=$candidate
        break
    fi
done

if [ -z "$SIZE" ]; then
    # Not fatal. The report is a convenience, and there is one setup it cannot
    # be made to work in: Windows `make` routing recipes through WSL bash, where
    # the Windows toolchain is reachable only through interop that may be off.
    # A build that stops because it could not print a table would be worse.
    echo "image-size: $TOOL not found; skipping size report" >&2
    echo "  (set XTENSA_SIZE, or run make from a shell that has the esp toolchain)" >&2
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
