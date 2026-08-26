#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Drive the owned-PIO self-test over SWD and judge the retained result. Needs a
# physical GP2->GP3 jumper: the program shifts words out on GP2 and counts the
# edges back in on GP3. Cross-platform port of rp2040-run-pio-selftest.ps1.
#
# Usage: rp2040-run-pio-selftest.sh <elf> <expected_hz> [serial_port]

set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/rp2040-swd-lib.sh"

ELF="${1:?usage: $0 <elf> <expected_hz> [serial_port]}"
EXPECTED_HZ="${2:?expected_hz must be 12000000 or 125000000}"
PORT="${3:-${FLINT_UART_PORT:-}}"
PY="${FLINT_PY:-python}"

case "$EXPECTED_HZ" in 12000000|125000000) ;; *) echo "expected_hz must be 12000000 or 125000000" >&2; exit 2;; esac

RESULTS="$(swd_addr "$ELF" PIO_RESULTS)"
NONCE_ADDR="$(swd_addr "$ELF" PIO_NONCE)"

CONSOLE="$(win_path "$(mktemp)")"; CAP_PID=""
cleanup() { if [ -n "$CAP_PID" ]; then kill "$CAP_PID" 2>/dev/null || true; wait "$CAP_PID" 2>/dev/null || true; fi; rm -f "$CONSOLE" "${_SWD_TMP:-}"; }
trap cleanup EXIT

if [ -n "$PORT" ]; then
    "$PY" "$(win_path "$HERE/rp2040-serial-capture.py")" --port "$PORT" --out "$CONSOLE" --seconds 50 &
    CAP_PID=$!
    sleep 0.5
else
    echo "WARNING: no serial port given; skipping the UART evidence assertion" >&2
fi

echo "downloading $ELF over SWD ..."
swd_download "$ELF"

for _ in $(seq 1 100); do
    W="$(swd_read "$RESULTS" 10)"
    [ "$(word "$W" 0)" -eq $((0x17500001)) ] && [ "$(word "$W" 1)" -eq 0 ] && break
    sleep 0.1
done
[ "$(word "$W" 0)" -eq $((0x17500001)) ] || { echo "FAIL: PIO test never reached its fresh-nonce gate (word0=$(printf 0x%08x $(word "$W" 0)))" >&2; exit 1; }

NONCE="$(fresh_nonce)"; NONCE_DEC=$((NONCE))
swd_write "$NONCE_ADDR" "$NONCE"
# The timeout/payload phases must run without a debugger repeatedly attaching.
sleep 6

for _ in $(seq 1 200); do
    W="$(swd_read "$RESULTS" 10)"
    w0="$(word "$W" 0)"
    [ "$w0" -eq $((0x1750600d)) ] && break
    [ $((w0 >> 16)) -eq $((0xbad1)) ] && break
    sleep 0.2
done

# ── Judge (mirrors Assert-Rp2040PioResult) ───────────────────────────────────
fail() { echo "FAIL: $1" >&2; echo "PIO words: $W" >&2; exit 1; }
[ "$(word "$W" 0)" -eq $((0x1750600d)) ] || fail "PIO result incomplete or failed (word0=$(printf 0x%08x $(word "$W" 0)))"
[ "$(word "$W" 1)" -eq "$NONCE_DEC" ] || fail "stale nonce (want $NONCE_DEC, got $(word "$W" 1))"
EXPECT=(2000 2 2 8 2 2 2)   # words[2..8]: words blocks timeout contention fifo-full fifo-empty reopen
for i in "${!EXPECT[@]}"; do
    idx=$((i + 2))
    [ "$(word "$W" "$idx")" -eq "${EXPECT[$i]}" ] || fail "PIO count word[$idx]=$(word "$W" "$idx") != ${EXPECT[$i]}"
done
d="$(abs_diff "$(word "$W" 9)" "$EXPECTED_HZ")"
[ "$d" -le 5000 ] || fail "cpu_hz word[9]=$(word "$W" 9) is $d Hz off $EXPECTED_HZ"

if [ -n "$PORT" ]; then
    sleep 0.3
    for line in \
        "[FLINT] PIO words=2000 blocks=2 timeout=2 contention=8" \
        "[FLINT] PIO fifo-full=2 fifo-empty=2 reopen=2"; do
        grep -qF "$line" "$CONSOLE" || fail "PIO UART evidence missing: $line"
    done
fi

echo "PASS: PIO ${EXPECTED_HZ}Hz  words=$(word "$W" 2) blocks=$(word "$W" 3) timeout=$(word "$W" 4) contention=$(word "$W" 5) fifo_full=$(word "$W" 6) fifo_empty=$(word "$W" 7) reopen=$(word "$W" 8) cpu_hz=$(word "$W" 9)  nonce=$NONCE_DEC"
