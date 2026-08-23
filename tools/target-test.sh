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
#   make test-target                          # flash and judge
#   make test-target BOARD=board-m5-atom      # an ESP32-PICO board
#   make test-target PORT=COM5                # name the port explicitly
#   make test-target APP=hello                # a different application
#   bash tools/target-test.sh --parse-only run.log
#
# Set PORT whenever more than one serial device is attached. Without it espflash
# asks which one to use, and this script has no terminal to answer with -- it
# would sit at the prompt until the timeout and then report a board that never
# started, which is a confusing way to say "pick a port".
#
# `--parse-only` exists so the judging logic can be tested without hardware,
# which is the half of this script most likely to be wrong. See its own tests in
# tools/target-test-selftest.sh.

set -uo pipefail

# A caller-supplied log path is relative to *their* cwd, so make it absolute
# before this script moves to the repo root.
if [ "${1:-}" = "--parse-only" ] && [ -n "${2:-}" ]; then
    case "$2" in
        /* | [A-Za-z]:[\\/]*) ;;             # already absolute
        *) set -- "$1" "$PWD/$2" ;;
    esac
fi

cd "$(dirname "$0")/.."

# Scratch files go in the repo, not /tmp.
#
# On Windows two MSYS-family runtimes are usually both installed -- MSYS2, which
# provides make and bash, and Git for Windows, which often provides the first
# mktemp on PATH -- and they map /tmp to different Windows directories. mktemp
# creates the file under its own mapping and prints a bare POSIX path; the shell
# then resolves that path somewhere else, and every write fails with "No such
# file or directory" naming a file that visibly just got created.
#
# `target/` is already gitignored and means one thing to both runtimes. Set
# before anything can use it -- `--parse-only` returns long before the rest of
# the setup runs.
WORK_ROOT="target/tmp"
mkdir -p "$WORK_ROOT"

MARK_BEGIN="[FLINT] SELFTEST BEGIN"
MARK_END="[FLINT] SELFTEST END"
MARK_TEST="[FLINT] TEST "

# The *complete* summary line, counts included, as an extended regex.
#
# The poll loop below waits for this rather than for MARK_END alone. The
# marker arrives a few bytes before "pass=N fail=N" does, so breaking on it and
# killing espflash immediately could truncate the line mid-way -- and the judge
# then failed a board that had passed, reporting that it could not read counts
# it had printed. Intermittent, because it depended on where the serial read
# happened to land.
MARK_SUMMARY='SELFTEST END pass=[0-9][0-9]* fail=[0-9][0-9]*'

# How long to let espflash keep draining after the summary appears.
#
# The line being complete does not mean the console is: the last test's PASS
# line and the summary can still be in flight behind it. A second costs nothing
# against a run that already took ten.
SETTLE_SECS=1

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
    log="$(mktemp "$WORK_ROOT/flint-judge.XXXXXX")"
    tr -d '\r' <"$raw" >"$log"
    # shellcheck disable=SC2064
    trap "rm -f '$log'" RETURN

    # Every grep below passes -a (treat the file as text). The capture almost
    # always contains one non-text byte -- the espflash reset garbles a handful
    # of bytes on the console before the ROM banner ("x\x1a...booting"). One such
    # byte flips grep into binary mode, where it prints "Binary file matches"
    # instead of the matching line: `end_line` comes back empty and every count
    # reads zero, so a clean 48/48 run is reported as "never finished". -a keeps
    # grep extracting the ASCII marker lines regardless of that boot noise.
    if ! grep -qaF "$MARK_BEGIN" "$log"; then
        echo "FAIL: the board never reached the self-test."
        echo "      No '$MARK_BEGIN' in the output — it may not have booted, or"
        echo "      the image may have been built without the self-test feature."
        return 1
    fi

    end_line=$(grep -aF "$MARK_END" "$log" | tail -1)
    if [ -z "$end_line" ]; then
        echo "FAIL: the self-test began but never finished."
        echo "      No '$MARK_END' — a test hung, the board reset, or it panicked"
        echo "      partway. The tests that did report are above; the first one"
        echo "      missing is where to look."
        return 1
    fi

    # Count what actually arrived, rather than trusting the summary. A dropped
    # line changes the count, and a run that silently lost a test is not a pass.
    passed=$(grep -caE "^.*${MARK_TEST//\[/\\[}.* PASS$" "$log" || true)
    failed=$(grep -caE "^.*${MARK_TEST//\[/\\[}.* FAIL " "$log" || true)

    # Bash's own regex, not `sed`.
    #
    # This shelled out to sed and failed *only when run from `make`*: the board
    # printed "pass=11 fail=0", the harness echoed that exact line back, and
    # then reported it could not read the counts from it. Reproducible five
    # times out of five through make, and never directly.
    #
    # The Makefile prepends Windows-style directories (`C:/...`) to PATH for
    # the toolchain. An MSYS shell splits PATH on `:`, so those entries mangle
    # the lookup: a recipe gets bash 5.2 and Git-for-Windows' sed where an
    # interactive shell gets bash 4.4 and MSYS2's, and that sed did not match
    # the pattern at all -- with or without a trailing CR.
    #
    # `[[ =~ ]]` is a builtin. No PATH lookup, nothing to pick the wrong copy
    # of. A test harness should not report a passing board as failed because of
    # which toolchain happens to be first on PATH.
    if [[ "$end_line" =~ pass=([0-9]+)[[:space:]]+fail=([0-9]+) ]]; then
        reported_pass="${BASH_REMATCH[1]}"
        reported_fail="${BASH_REMATCH[2]}"
    else
        reported_pass=""
        reported_fail=""
    fi

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
        grep -aF "$MARK_TEST" "$log" | grep -aF " FAIL " | sed 's/^/      /'
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

APP="${APP:-demo}"
BOARD="${BOARD:-board-esp32-wrover}"
DEBUG="${DEBUG:-debug-level-1}"
ESPFLASH_CHIP="${ESPFLASH_CHIP:-esp32}"
FLASH_MODE="${FLASH_MODE:-dio}"
FLASH_BAUD="${FLASH_BAUD:-115200}"
MONITOR_BAUD="${MONITOR_BAUD:-115200}"
BIN="target/xtensa-esp32-none-elf/debug/${APP}"

# Locate espflash without trusting PATH alone.
#
# cargo installs it to ~/.cargo/bin and adds that to the *persisted* PATH — but
# a shell opened before the install keeps the PATH it inherited. The result is
# `make test-target` failing with "not found" in one terminal and working in
# another, which reads as a broken Makefile rather than a stale session. Look in
# the places it is actually installed.
ESPFLASH=""
CANDIDATES=("espflash")

# Passed down by the Makefile, which resolves it from `rustup show home`.
[ -n "${CARGO_BIN_DIR:-}" ] && CANDIDATES+=("$CARGO_BIN_DIR/espflash")
[ -n "${CARGO_HOME:-}" ] && CANDIDATES+=("$CARGO_HOME/bin/espflash")

# Standalone fallback: ask rustup where it lives and take .cargo as its sibling.
# This is the only reliable way to reach the Windows profile from a recipe --
# MSYS2's make strips USERPROFILE, and $HOME there is the MSYS home
# (/home/<user>), not the profile cargo installed into. An earlier version of
# this list relied on those two and so searched exactly one wrong directory.
if rustup_home="$(rustup show home 2>/dev/null)" && [ -n "$rustup_home" ]; then
    rustup_home="${rustup_home//\\//}"
    CANDIDATES+=("${rustup_home%/.rustup}/.cargo/bin/espflash")
fi

[ -n "${HOME:-}" ] && CANDIDATES+=("$HOME/.cargo/bin/espflash")
if [ -n "${USERPROFILE:-}" ] && command -v cygpath >/dev/null 2>&1; then
    CANDIDATES+=("$(cygpath -u "$USERPROFILE")/.cargo/bin/espflash")
fi

for candidate in "${CANDIDATES[@]}"; do
    if [ "$candidate" = "espflash" ]; then
        # Bare name: a PATH lookup, which on Windows resolves the .exe itself.
        if resolved="$(command -v espflash 2>/dev/null)" && [ -n "$resolved" ]; then
            ESPFLASH="$resolved"
            break
        fi
        continue
    fi
    # Explicit path: `command -v` does NOT apply the .exe suffix here, so the
    # file must be probed both ways or the fallback silently finds nothing —
    # which is the whole failure this block exists to prevent.
    for path in "$candidate" "$candidate.exe"; do
        if [ -x "$path" ]; then
            ESPFLASH="$path"
            break 2
        fi
    done
done

# Are we inside WSL? If so, nothing installed on Windows is reachable and no
# COM port exists here, so no amount of PATH fixing will help. Worth detecting
# because it is easy to land in by accident: C:\Windows\System32\bash.exe is the
# WSL launcher and comes ahead of MSYS2 and Git Bash on a default PATH, so a
# `bash tools/...` typed at a Windows prompt runs the script in a Linux distro.
in_wsl() {
    [ -n "${WSL_DISTRO_NAME:-}" ] && return 0
    grep -qi microsoft /proc/version 2>/dev/null
}

wrong_shell_hint() {
    if in_wsl; then
        echo
        echo "  This shell is WSL${WSL_DISTRO_NAME:+ ($WSL_DISTRO_NAME)}, and the toolchain is installed on"
        echo "  Windows. WSL cannot see it, and cannot open a COM port either."
        echo "  Run this from Git Bash, the MSYS2 shell, or PowerShell instead:"
        echo "      make test-target BOARD=$BOARD PORT=$PORT"
        echo
        echo "  (On Windows, bare \`bash\` resolves to C:\\Windows\\System32\\bash.exe --"
        echo "   the WSL launcher. The Makefile pins \$(BASH) to avoid exactly this.)"
    fi
}

if [ -z "$ESPFLASH" ]; then
    {
        echo "espflash not found."
        echo "  Looked on PATH and in:"
        for candidate in "${CANDIDATES[@]:1}"; do echo "    $candidate"; done
        wrong_shell_hint
        if ! in_wsl; then
            echo
            echo "  Install it with:  cargo install espflash"
            echo "  If it IS installed, this shell's PATH predates the install."
            echo "  Open a new terminal, or:  export PATH=\"\$HOME/.cargo/bin:\$PATH\""
        fi
    } >&2
    exit 2
fi

# The build needs cargo, which goes missing for the same reason espflash does.
# Check before building rather than letting `cargo +esp build` fail with a bare
# "command not found" that names neither the cause nor the fix.
if ! command -v cargo >/dev/null 2>&1; then
    {
        echo "cargo not found."
        wrong_shell_hint
        if ! in_wsl; then
            echo "  Install Rust, or add ~/.cargo/bin to PATH."
        fi
    } >&2
    exit 2
fi

echo "==> Building ${APP} with the self-test suite"
# Board and debug are kernel features passed on the command line (#120); only
# `self-test` is the app's own.
cargo +esp build \
    --target xtensa-esp32-none-elf \
    -Z build-std=core,compiler_builtins \
    -p "$APP" --no-default-features \
    --features "kernel/${BOARD},kernel/${DEBUG},self-test" || exit 1

LOG="$(mktemp "$WORK_ROOT/flint-target-test.XXXXXX")"
trap 'rm -f "$LOG"' EXIT

echo "==> Flashing and capturing (timeout ${TIMEOUT_SECS}s)"

# espflash --monitor never returns on its own, so it runs in the background and
# is killed once the terminating marker arrives or the timeout expires. Polling
# the log beats piping into `read`, which would leave espflash orphaned holding
# the serial port open — and the next run would then fail to open it.
PORT_ARGS=()
if [ -n "${PORT:-}" ]; then
    PORT_ARGS=(--port "$PORT")
fi

"$ESPFLASH" flash "$BIN" \
    --chip "$ESPFLASH_CHIP" --flash-mode "$FLASH_MODE" \
    --baud "$FLASH_BAUD" --monitor --monitor-baud "$MONITOR_BAUD" \
    "${PORT_ARGS[@]}" \
    >"$LOG" 2>&1 &
ESPFLASH_PID=$!

deadline=$((SECONDS + TIMEOUT_SECS))
while kill -0 "$ESPFLASH_PID" 2>/dev/null; do
    if grep -qaE "$MARK_SUMMARY" "$LOG" 2>/dev/null; then
        # Let the tail of the output land before killing the monitor.
        sleep "$SETTLE_SECS"
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

# Keep the raw capture for diagnosis. The judge reads bytes that the indented
# echo below can hide -- carriage returns from espflash's progress bar, escape
# sequences -- so "it looked fine on screen" is not evidence about what was
# parsed.
if [ -n "${FLINT_KEEP_LOG:-}" ]; then
    cp "$LOG" "$FLINT_KEEP_LOG"
    echo "==> Raw capture kept at $FLINT_KEEP_LOG"
fi

echo
echo "==> Board output"
sed 's/^/    /' "$LOG"
echo
judge "$LOG"
