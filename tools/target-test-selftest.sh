#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Tests for the judging half of tools/target-test.sh.
#
# The parser is the part of the on-target harness most likely to be quietly
# wrong, and the part hardest to exercise — reaching it normally costs a board,
# a flash cycle and a minute. Feeding it captured logs instead makes its failure
# modes cheap to check, and those are what matter: a harness that reports a
# green run when the serial line dropped half the output is worse than no
# harness, because it manufactures confidence.
#
#   bash tools/target-test-selftest.sh

set -uo pipefail
cd "$(dirname "$0")/.."

HARNESS="tools/target-test.sh"

# An explicit directory inside the repo, not `mktemp -t`, which routes through
# /tmp.
#
# On Windows two MSYS-family runtimes are usually both installed -- MSYS2, which
# provides make and bash, and Git for Windows, which often provides the first
# mktemp on PATH -- and they map /tmp to different Windows directories. mktemp
# then creates the directory under its own mapping and prints a bare POSIX path;
# the shell resolves that same path somewhere else, and every write fails with
# "No such file or directory" naming a directory that visibly just got created.
#
# `target/` is already gitignored and means one thing to both runtimes.
#
# Not named TMP: make exports that as a native path so the linkers it invokes
# can find a writable directory, and a bash assignment to an imported name stays
# exported. Overwriting it here would hand every child a POSIX path it cannot use.
WORK_ROOT="target/tmp"
mkdir -p "$WORK_ROOT"
WORK="$(mktemp -d "$WORK_ROOT/flint-harness-tests.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

failures=0

# expect <name> <expected-exit> <log-content>
expect() {
    local name="$1" want="$2" body="$3"
    local log="$WORK/$name.log"
    printf '%s' "$body" >"$log"

    local out rc
    out="$(bash "$HARNESS" --parse-only "$log" 2>&1)"
    rc=$?

    if [ "$rc" -eq "$want" ]; then
        printf '  ok    %-34s (exit %d)\n' "$name" "$rc"
    else
        printf '  FAIL  %-34s (exit %d, wanted %d)\n' "$name" "$rc" "$want"
        printf '%s\n' "$out" | sed 's/^/          /'
        failures=$((failures + 1))
    fi
}

echo "Testing the on-target harness's judging logic"

expect all_pass 0 '[FLINT] SELFTEST BEGIN
[FLINT] TEST timer_preserves_windowed_context PASS
[FLINT] TEST tick_advances PASS
[FLINT] SELFTEST END pass=2 fail=0
'

# The realistic capture: CRLF from the serial console. If the CR is not
# stripped, every anchored match fails and this reads as "no tests ran".
printf 'crlf test uses explicit CRLF below\n' >/dev/null
expect crlf_line_endings 0 "$(printf '[FLINT] SELFTEST BEGIN\r\n[FLINT] TEST tick_advances PASS\r\n[FLINT] SELFTEST END pass=1 fail=0\r\n')"

expect one_failure 1 '[FLINT] SELFTEST BEGIN
[FLINT] TEST tick_advances PASS
[FLINT] TEST critical_section_masks_the_tick FAIL tick advanced while masked
[FLINT] SELFTEST END pass=1 fail=1
'

# Board never booted, or the image lacked the feature.
expect never_started 1 'boot log with no self-test at all
'

# Hung or reset partway: tests reported, no summary.
expect truncated_no_end 1 '[FLINT] SELFTEST BEGIN
[FLINT] TEST tick_advances PASS
'

# The important one. The board says two passed; only one line survived the
# wire. Trusting the summary would call this green.
expect dropped_line_counts_disagree 1 '[FLINT] SELFTEST BEGIN
[FLINT] TEST tick_advances PASS
[FLINT] SELFTEST END pass=2 fail=0
'

# A suite that ran nothing is not a suite that passed.
expect zero_tests 1 '[FLINT] SELFTEST BEGIN
[FLINT] SELFTEST END pass=0 fail=0
'

# Garbled summary — unreadable counts must not be read as zero failures.
expect unparseable_summary 1 '[FLINT] SELFTEST BEGIN
[FLINT] TEST tick_advances PASS
[FLINT] SELFTEST END pass=?? fail=~~
'

# Real boards print other things. Surrounding noise must not confuse the parse.
expect noise_around_the_markers 0 '[FLINT] boot: console up
rst:0x1 (POWERON_RESET),boot:0x13
[FLINT] SELFTEST BEGIN
[FLINT] TEST tick_advances PASS
[FLINT] SELFTEST END pass=1 fail=0
[FLINT] idle
'

echo
if [ "$failures" -eq 0 ]; then
    echo "All harness tests passed."
    exit 0
fi
echo "$failures harness test(s) failed."
exit 1
