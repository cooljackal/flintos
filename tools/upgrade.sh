#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Pull the latest FlintOS and report which applications it broke.
#
# The layout already protects application code: applications are separate
# crates and a pull never touches `apps/*/<yours>/`. What it does not do is tell
# you what the pull changed underneath them, and "it still compiles" is the
# only question that matters at that moment.
#
# So: pull, rebuild every application, and say which ones stopped building --
# with the first error and the changelog entries that landed. A wall of
# compiler output is not an upgrade report.
#
#   make upgrade                 # pull, then check
#   make upgrade PULL=0          # check against what is already checked out
#
# Exits non-zero if any application broke, so this is usable in a script.

set -uo pipefail
cd "$(dirname "$0")/.."

PULL="${PULL:-1}"
BOARD="${BOARD:-board-esp32-wrover}"
DEBUG="${DEBUG:-debug-level-1}"
CARGO="${CARGO:-cargo +esp}"
XTENSA_TARGET="${XTENSA_TARGET:-xtensa-esp32-none-elf}"

bold=$'\033[1m'; red=$'\033[31m'; green=$'\033[32m'; dim=$'\033[2m'; off=$'\033[0m'
if [ ! -t 1 ]; then bold=""; red=""; green=""; dim=""; off=""; fi

WORK_ROOT="target/tmp"
mkdir -p "$WORK_ROOT"

# ── Find the applications ───────────────────────────────────────────────────
#
# Whatever is in apps/examples/ and apps/tests/, rather than a list kept here.
# A list would go stale the first time someone adds an application and forgets,
# and this reporting a clean upgrade because it never looked is the one outcome
# to avoid. The glob is two levels deep because that is where the crates are;
# one level would match only the two group directories, find no Cargo.toml,
# and exit 0 with "No applications" -- a quiet pass, the very thing above.
APPS=()
for d in apps/*/*/; do
    [ -f "$d/Cargo.toml" ] || continue
    APPS+=("$(basename "$d")")
done

if [ "${#APPS[@]}" -eq 0 ]; then
    echo "No applications in apps/examples/ or apps/tests/ -- nothing to check." >&2
    exit 0
fi

# ── Pull ────────────────────────────────────────────────────────────────────

BEFORE="$(git rev-parse HEAD 2>/dev/null || echo unknown)"

if [ "$PULL" = "1" ]; then
    if ! git diff --quiet || ! git diff --cached --quiet; then
        echo "${red}Working tree has uncommitted changes.${off}" >&2
        echo "Commit or stash them first -- an upgrade that merges into dirty" >&2
        echo "state is hard to unpick when it goes wrong." >&2
        exit 1
    fi
    echo "${bold}==> Pulling${off}"
    if ! git pull --ff-only; then
        echo >&2
        echo "${red}Pull failed.${off} A fast-forward was not possible, which usually" >&2
        echo "means local commits. Rebase or merge by hand, then re-run with PULL=0." >&2
        exit 1
    fi
else
    echo "${dim}==> Skipping pull (PULL=0)${off}"
fi

AFTER="$(git rev-parse HEAD 2>/dev/null || echo unknown)"

echo
if [ "$BEFORE" = "$AFTER" ]; then
    echo "${bold}Already up to date${off} (${AFTER:0:7})."
else
    echo "${bold}Updated${off} ${BEFORE:0:7} -> ${AFTER:0:7}"
fi

# ── What changed that an application can see ────────────────────────────────

if [ "$BEFORE" != "$AFTER" ] && [ "$BEFORE" != unknown ]; then
    api_changed="$(git diff --name-only "$BEFORE" "$AFTER" -- api hal board 2>/dev/null)"
    if [ -n "$api_changed" ]; then
        echo
        echo "${bold}The application-facing API changed:${off}"
        echo "$api_changed" | sed 's/^/  /'
    fi

    # The Breaking entries are the actionable part; print them rather than
    # asking someone to go and find the file.
    if git diff --quiet "$BEFORE" "$AFTER" -- CHANGELOG.md 2>/dev/null; then
        :
    else
        echo
        echo "${bold}CHANGELOG.md changed. Breaking entries now present:${off}"
        awk '/^### Breaking/{f=1;next} /^### /{f=0} f' CHANGELOG.md \
            | sed '/^[[:space:]]*$/d' | head -40 | sed 's/^/  /'
    fi
fi

# ── Rebuild every application ───────────────────────────────────────────────

echo
echo "${bold}==> Rebuilding ${#APPS[@]} application(s)${off}  ${dim}(BOARD=$BOARD DEBUG=$DEBUG)${off}"

BROKEN=()
for app in "${APPS[@]}"; do
    log="$WORK_ROOT/upgrade-$app.log"
    printf '  %-14s ' "$app"
    if $CARGO build --target "$XTENSA_TARGET" \
        -Z build-std=core,compiler_builtins \
        -p "$app" --no-default-features \
        --features "$BOARD,$DEBUG" >"$log" 2>&1
    then
        echo "${green}ok${off}"
    else
        echo "${red}BROKEN${off}"
        BROKEN+=("$app")
    fi
done

if [ "${#BROKEN[@]}" -eq 0 ]; then
    echo
    echo "${green}${bold}All ${#APPS[@]} application(s) still build.${off}"
    exit 0
fi

# ── Report ──────────────────────────────────────────────────────────────────
#
# The first error, not all of them. After a breaking API change every later
# error is usually the same cause repeated, and a screen of them buries the one
# worth reading.

echo
echo "${red}${bold}${#BROKEN[@]} application(s) broke.${off}"
for app in "${BROKEN[@]}"; do
    log="$WORK_ROOT/upgrade-$app.log"
    echo
    echo "${bold}--- $app ---${off}"

    # An ABI mismatch is the expected, well-diagnosed case: show it and stop,
    # because every other error is downstream of it.
    if grep -q "FlintOS ABI mismatch" "$log"; then
        sed -n '/FlintOS ABI mismatch/,/^$/p' "$log" | head -12 | sed 's/^/  /'
        echo "  ${dim}Apply the Breaking entries above, then bump the abi in flint_app!.${off}"
        continue
    fi

    awk '/^error/{c++} c==1{print} c>1{exit}' "$log" | head -20 | sed 's/^/  /'
    echo "  ${dim}Full output: $log${off}"
done

echo
echo "Read the Breaking entries in CHANGELOG.md; each says what to change."
exit 1
