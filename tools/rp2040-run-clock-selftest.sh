#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Measure the Pico CPU clock on both cores over SWD and judge the retained
# result. Cross-platform port of rp2040-run-clock-selftest.ps1: bash + probe-rs
# for the measurement, a small pyserial helper for the boot-provenance line.
#
# Usage: rp2040-run-clock-selftest.sh <elf> <expected_hz> [serial_port]
#   FLINT_PROBE_SERIAL  debug-probe USB serial (required)
#   expected_hz         12000000 or 125000000

set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/rp2040-swd-lib.sh"

ELF="${1:?usage: $0 <elf> <expected_hz> [serial_port]}"
EXPECTED_HZ="${2:?expected_hz must be 12000000 or 125000000}"
PORT="${3:-${FLINT_UART_PORT:-}}"
PY="${FLINT_PY:-python}"

case "$EXPECTED_HZ" in 12000000|125000000) ;; *) echo "expected_hz must be 12000000 or 125000000" >&2; exit 2;; esac

RESULTS="$(swd_addr "$ELF" CLOCK_RESULTS)"
NONCE_ADDR="$(swd_addr "$ELF" CLOCK_NONCE)"

CONSOLE="$(win_path "$(mktemp)")"; CAP_PID=""
cleanup() { if [ -n "$CAP_PID" ]; then kill "$CAP_PID" 2>/dev/null || true; wait "$CAP_PID" 2>/dev/null || true; fi; rm -f "$CONSOLE" "${_SWD_TMP:-}"; }
trap cleanup EXIT

if [ -n "$PORT" ]; then
    "$PY" "$(win_path "$HERE/rp2040-serial-capture.py")" --port "$PORT" --out "$CONSOLE" --seconds 45 &
    CAP_PID=$!
    sleep 0.5
else
    echo "WARNING: no serial port given; skipping the boot-provenance (UART) assertion" >&2
fi

echo "downloading $ELF over SWD ..."
swd_download "$ELF"

# Wait for the fresh-nonce gate: word0 = 0x17400001, word1 = 0.
for _ in $(seq 1 100); do
    W="$(swd_read "$RESULTS" 11)"
    [ "$(word "$W" 0)" -eq $((0x17400001)) ] && [ "$(word "$W" 1)" -eq 0 ] && break
    sleep 0.1
done
[ "$(word "$W" 0)" -eq $((0x17400001)) ] || { echo "FAIL: clock test never reached its fresh-nonce gate (word0=$(printf 0x%08x $(word "$W" 0)))" >&2; exit 1; }

NONCE="$(fresh_nonce)"; NONCE_DEC=$((NONCE))
swd_write "$NONCE_ADDR" "$NONCE"

# Quiet window: the sampling and SysTick check must run without the debugger
# polling, because RP2040 TIMER_DBGPAUSE halts the timer while a core is halted.
sleep 3

for _ in $(seq 1 200); do
    W="$(swd_read "$RESULTS" 11)"
    w0="$(word "$W" 0)"
    [ "$w0" -eq $((0x1740600d)) ] && break
    [ $((w0 >> 16)) -eq $((0xbad0)) ] && break
    sleep 0.1
done

# ── Judge (mirrors Assert-Rp2040ClockResult) ─────────────────────────────────
fail() { echo "FAIL: $1" >&2; echo "clock words: $W" >&2; exit 1; }
[ "$(word "$W" 0)" -eq $((0x1740600d)) ] || fail "clock result incomplete or failed (word0=$(printf 0x%08x $(word "$W" 0)))"
[ "$(word "$W" 1)" -eq "$NONCE_DEC" ] || fail "stale nonce (want $NONCE_DEC, got $(word "$W" 1))"
[ "$(word "$W" 2)" -eq "$EXPECTED_HZ" ] || fail "configured_hz $(word "$W" 2) != $EXPECTED_HZ"
for i in 3 4 5; do
    d="$(abs_diff "$(word "$W" $i)" "$EXPECTED_HZ")"
    [ "$d" -le 5000 ] || fail "word[$i]=$(word "$W" $i) is $d Hz off $EXPECTED_HZ"
done
[ "$(word "$W" 4)" -le "$(word "$W" 5)" ] || fail "min_hz > max_hz"
[ "$(word "$W" 6)" -eq 32 ] || fail "core0 samples $(word "$W" 6) != 32"
[ "$(word "$W" 7)" -eq 32 ] || fail "core1 samples $(word "$W" 7) != 32"
eu="$(word "$W" 9)"; [ "$eu" -ge 90000 ] && [ "$eu" -le 120000 ] || fail "elapsed_us $eu out of [90000,120000]"
et="$(word "$W" 10)"; [ "$et" -ge 90 ] && [ "$et" -le 120 ] || fail "elapsed_ticks $et out of [90,120]"

if [ -n "$PORT" ]; then
    sleep 0.3
    BOOT_LINE="[FLINT] cpu_hz=$(word "$W" 3) (measured against crystal-backed reference)"
    grep -qF "$BOOT_LINE" "$CONSOLE" || fail "boot did not report the measured clock over UART (looked for: $BOOT_LINE)"
    grep -qF "ASSUMED:" "$CONSOLE" && fail "boot reported an ASSUMED clock"
fi

echo "PASS: clock ${EXPECTED_HZ}Hz  boot_hz=$(word "$W" 3) min=$(word "$W" 4) max=$(word "$W" 5) core0=$(word "$W" 6) core1=$(word "$W" 7) elapsed_us=$eu ticks=$et  nonce=$NONCE_DEC"
