#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Prove MPU denial and per-core domain switching on the Pico over SWD, and judge
# the retained result. Cross-platform port of the standard (non-fault) path of
# rp2040-run-isolation-selftest.ps1.
#
# The unexpected-HardFault variant is not ported here: it proves a retained
# panic survives a reboot, which needs the target's own USB (ROM BOOTSEL) to
# reflash between boots. This harness reaches the target only over SWD.
#
# Usage: rp2040-run-isolation-selftest.sh <elf> [serial_port]

set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/rp2040-swd-lib.sh"

ELF="${1:?usage: $0 <elf> [serial_port]}"
PORT="${2:-${FLINT_UART_PORT:-}}"
PY="${FLINT_PY:-python}"

RESULTS="$(swd_addr "$ELF" ISOLATION_RESULTS)"
NONCE_ADDR="$(swd_addr "$ELF" ISOLATION_NONCE)"

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
    W="$(swd_read "$RESULTS" 8)"
    [ "$(word "$W" 0)" -eq $((0x13900001)) ] && [ "$(word "$W" 1)" -eq 0 ] && break
    sleep 0.1
done
[ "$(word "$W" 0)" -eq $((0x13900001)) ] || { echo "FAIL: isolation test never reached its fresh-nonce gate (word0=$(printf 0x%08x $(word "$W" 0)))" >&2; exit 1; }

NONCE="$(fresh_nonce)"; NONCE_DEC=$((NONCE))
swd_write "$NONCE_ADDR" "$NONCE"
sleep 6

for _ in $(seq 1 200); do
    W="$(swd_read "$RESULTS" 8)"
    w0="$(word "$W" 0)"
    [ "$w0" -eq $((0x1390600d)) ] && break
    [ $((w0 >> 16)) -eq $((0xbad1)) ] && break
    sleep 0.2
done

# ── Judge (mirrors Assert-Rp2040IsolationResult) ─────────────────────────────
fail() { echo "FAIL: $1" >&2; echo "isolation words: $W" >&2; exit 1; }
[ "$(word "$W" 0)" -eq $((0x1390600d)) ] || fail "isolation result incomplete or failed (word0=$(printf 0x%08x $(word "$W" 0)))"
[ "$(word "$W" 1)" -eq "$NONCE_DEC" ] || fail "stale nonce (want $NONCE_DEC, got $(word "$W" 1))"
[ "$(word "$W" 2)" -eq 24 ]  || fail "MPU faults $(word "$W" 2) != 24"
[ "$(word "$W" 3)" -eq 3 ]   || fail "rejected $(word "$W" 3) != 3"
[ "$(word "$W" 4)" -eq 800 ] || fail "iterations $(word "$W" 4) != 800"
[ "$(word "$W" 5)" -eq 2 ]   || fail "cores $(word "$W" 5) != 2"
[ "$(word "$W" 6)" -ge 200 ] || fail "core0 activations $(word "$W" 6) < 200"
[ "$(word "$W" 7)" -ge 200 ] || fail "core1 activations $(word "$W" 7) < 200"

if [ -n "$PORT" ]; then
    sleep 0.3
    grep -qF "[FLINT] MPU faults=24 rejected=3 iterations=800 cores=2" "$CONSOLE" \
        || fail "isolation UART evidence missing"
fi

echo "PASS: isolation  faults=$(word "$W" 2) rejected=$(word "$W" 3) iterations=$(word "$W" 4) cores=$(word "$W" 5) act_core0=$(word "$W" 6) act_core1=$(word "$W" 7)  nonce=$NONCE_DEC"
