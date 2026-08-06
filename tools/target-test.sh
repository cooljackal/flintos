#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Run the on-target self-tests on a real board and turn the serial output into
# an exit code.
#
# The host suite runs on every change and covers the kernel's logic against the
# stand-ins in `kernel::arch`. This covers what those stand-ins cannot: register
# windows spilled across a trap, a critical section that genuinely masks the
# timer, a tick that advances because silicon counts. It needs a board plugged
# in, so it is user-initiated rather than part of CI.
#
#   make test-target                     # flash and judge
#   make test-target APP=hello           # a different application
#   bash tools/target-test.sh --parse-only run.log
#
# `--parse-only` exists so the judging logic can be tested without hardware,
# which is the half of this script most likely to be wrong. See its own tests in
# tools/target-test-selftest.sh.

set -uo pipefail

MARK_BEGIN="[FLINT] SELFTEST BEGIN"
MARK_END="[FLINT] SELFTEST END"
MARK_TEST="[FLINT] TEST "

# Long enough for a slow flash plus the tests themselves (the tick tests spin
# for several tick periods, the recursion tests are bounded). Short enough that
# a wedged board does not hold a terminal forever.
TIMEOUT_SECS="${TIMEOUT_SECS:-90}"

# ── Judging ─────────────────────────────────────────────────────────────────

# Decide the outcome from a captured log. Deliberately strict: a serial line
# that never arrived must not read as a pass.
judge() {
    local raw="$1"
    local log passed failed reported_pass reported_fail end_line

    # The board terminates lines with CRLF, as a serial console should. Left in
    # place, the trailing CR sits between "PASS" and end-of-line and every
    # anchored match below silently fails — the harness would then report a
    # clean board as having run no tests at all. Strip it once, here.
    log="$(mktemp -t flint-judge.XXXXXX)"
    tr -d '\r' <"$raw" >"$log"
    # shellcheck disable=SC2064
    trap "rm -f '$log'" RETURN

    if ! grep -qF "$MARK_BEGIN" "$log"; then
        echo "FAIL: the board never reached the self-test."
        echo "      No '$MARK_BEGIN' in the output — it may not have booted, or"
        echo "      the image may have been built without the self-test feature."
        return 1
    fi

    end_line=$(grep -F "$MARK_END" "$log" | tail -1)
    if [ -z "$end_line" ]; then
        echo "FAIL: the self-test began but never finished."
        echo "      No '$MARK_END' — a test hung, the board reset, or it panicked"
        echo "      partway. The tests that did report are above; the first one"
        echo "      missing is where to look."
        return 1
    fi

    # Count what actually arrived, rather than trusting the summary. A dropped
    # line changes the count, and a run that silently lost a test is not a pass.
    passed=$(grep -cE "^.*${MARK_TEST//\[/\\[}.* PASS$" "$log" || true)
    failed=$(grep -cE "^.*${MARK_TEST//\[/\\[}.* FAIL " "$log" || true)

    reported_pass=$(sed -n 's/.*pass=\([0-9][0-9]*\).*/\1/p' <<<"$end_line")
    reported_fail=$(sed -n 's/.*fail=\([0-9][0-9]*\).*/\1/p' <<<"$end_line")

    if [ -z "$reported_pass" ] || [ -z "$reported_fail" ]; then
        echo "FAIL: could not read the summary counts from: $end_line"
        return 1
    fi

    if [ "$passed" -ne "$reported_pass" ] || [ "$failed" -ne "$reported_fail" ]; then
        echo "FAIL: the board reported pass=$reported_pass fail=$reported_fail,"
        echo "      but only $passed PASS and $failed FAIL lines arrived intact."
        echo "      Serial dropped or garbled output — treat this run as void,"
        echo "      not as a result. A lower baud (MONITOR_BAUD) often fixes it."
        return 1
    fi

    if [ "$reported_fail" -ne 0 ]; then
        echo "FAIL: $reported_fail of $((reported_pass + reported_fail)) on-target tests failed:"
        grep -F "$MARK_TEST" "$log" | grep -F " FAIL " | sed 's/^/      /'
        return 1
    fi

    if [ "$reported_pass" -eq 0 ]; then
        echo "FAIL: the suite reported no tests at all."
        echo "      An empty run is not a passing run."
        return 1
    fi

    echo "PASS: $reported_pass on-target tests passed."
    return 0
}

# ── Entry ───────────────────────────────────────────────────────────────────

if [ "${1:-}" = "--parse-only" ]; then
    [ -n "${2:-}" ] || { echo "usage: $0 --parse-only <logfile>" >&2; exit 2; }
    [ -r "${2}" ] || { echo "cannot read ${2}" >&2; exit 2; }
    judge "$2"
    exit $?
fi

cd "$(dirname "$0")/.."

APP="${APP:-demo}"
BOARD="${BOARD:-board-esp32-wrover}"
DEBUG="${DEBUG:-debug-level-1}"
ESPFLASH_CHIP="${ESPFLASH_CHIP:-esp32}"
FLASH_MODE="${FLASH_MODE:-dio}"
FLASH_BAUD="${FLASH_BAUD:-115200}"
MONITOR_BAUD="${MONITOR_BAUD:-115200}"
BIN="target/xtensa-esp32-none-elf/debug/${APP}"

command -v espflash >/dev/null 2>&1 || {
    echo "espflash not found. Install it with: cargo install espflash" >&2
    exit 2
}

echo "==> Building ${APP} with the self-test suite"
cargo +esp build \
    --target xtensa-esp32-none-elf \
    -Z build-std=core,compiler_builtins \
    -p "$APP" --no-default-features \
    --features "${BOARD},${DEBUG},self-test" || exit 1

LOG="$(mktemp -t flint-target-test.XXXXXX)"
trap 'rm -f "$LOG"' EXIT

echo "==> Flashing and capturing (timeout ${TIMEOUT_SECS}s)"

# espflash --monitor never returns on its own, so it runs in the background and
# is killed once the terminating marker arrives or the timeout expires. Polling
# the log beats piping into `read`, which would leave espflash orphaned holding
# the serial port open — and the next run would then fail to open it.
espflash flash "$BIN" \
    --chip "$ESPFLASH_CHIP" --flash-mode "$FLASH_MODE" \
    --baud "$FLASH_BAUD" --monitor --monitor-baud "$MONITOR_BAUD" \
    >"$LOG" 2>&1 &
ESPFLASH_PID=$!

deadline=$((SECONDS + TIMEOUT_SECS))
while kill -0 "$ESPFLASH_PID" 2>/dev/null; do
    if grep -qF "$MARK_END" "$LOG" 2>/dev/null; then
        break
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
        echo "==> Timed out after ${TIMEOUT_SECS}s"
        break
    fi
    sleep 1
done

kill "$ESPFLASH_PID" 2>/dev/null
wait "$ESPFLASH_PID" 2>/dev/null

echo
echo "==> Board output"
sed 's/^/    /' "$LOG"
echo
judge "$LOG"
