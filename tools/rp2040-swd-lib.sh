# SPDX-License-Identifier: Apache-2.0
# Shared SWD helpers for the RP2040 on-target self-tests.
#
# Every operation goes through `probe-rs` over the CMSIS-DAP debug probe, so the
# whole flow is the same on Windows, macOS and Linux -- no PowerShell, no
# platform-specific flashing path. Source this from a `-selftest.sh` runner
# after setting FLINT_PROBE_SERIAL (the probe's USB serial).
#
# The RP2040 answers SWD at 100 kHz; the default is faster than a bare Pico wired
# to a probe over jumper leads reliably sustains, and a dropped clock edge during
# a download shows up as a verify failure, not a retry.

set -euo pipefail

: "${FLINT_PROBE_SERIAL:?set FLINT_PROBE_SERIAL to the debug probe USB serial}"
PROBE="2e8a:000c:${FLINT_PROBE_SERIAL}"
SWD_SPEED="${FLINT_SWD_SPEED:-100}"

# Scratch file for probe-rs read output. Created once here in the parent shell
# and exported, so the command-substitution subshells that call swd_read all
# share the one path instead of each minting (and leaking) their own. On Windows
# it must be a native path: probe-rs is a native exe and, under make, cannot
# write through a redirect to an MSYS-root path (/tmp/...); cygpath rewrites it
# to the equivalent Windows path (same file). A no-op elsewhere.
if [ -z "${_SWD_TMP:-}" ]; then
    _SWD_TMP="$(mktemp)"
    command -v cygpath >/dev/null 2>&1 && _SWD_TMP="$(cygpath -m "$_SWD_TMP")"
    export _SWD_TMP
fi

# llvm-nm ships inside the active rustc sysroot, so it always matches the
# toolchain that built the image and needs nothing extra on PATH.
_LLVM_NM=""
swd_nm() {
    if [ -z "$_LLVM_NM" ]; then
        local sysroot d
        # rustc prints a native path; on Windows that means backslashes, which
        # bash's test builtin treats as literal characters rather than
        # separators. Normalise with pure parameter expansion -- piping rustc
        # into tr/sed intermittently fails under `make` on Windows ("pipe is
        # being closed"), so keep this pipe-free. Glob for llvm-nm under the one
        # host dir in lib/rustlib rather than deriving the host triple, which
        # would need a second (fragile) rustc invocation.
        sysroot="$(rustc --print sysroot)"
        sysroot="${sysroot%$'\r'}"
        sysroot="${sysroot//\\//}"
        for d in "$sysroot"/lib/rustlib/*/bin/llvm-nm.exe "$sysroot"/lib/rustlib/*/bin/llvm-nm; do
            [ -x "$d" ] && { _LLVM_NM="$d"; break; }
        done
        if [ -z "$_LLVM_NM" ]; then
            command -v llvm-nm >/dev/null 2>&1 && _LLVM_NM="llvm-nm"
        fi
        [ -n "$_LLVM_NM" ] || { echo "llvm-nm not found under $sysroot" >&2; return 1; }
    fi
    "$_LLVM_NM" "$@"
}

# Print the address of exactly one symbol, 0x-prefixed. A test that read the
# wrong symbol -- or an image where a rename left two -- must fail loudly, not
# silently sample the wrong memory.
# The symbol table is captured once. Piping the native llvm-nm.exe straight
# into awk drops its output under `make` on Windows (the same broken
# native-exe-into-pipe path that breaks rustc), so capture first, then parse the
# variable.
_NM_DUMP=""
swd_addr() {
    local elf="$1" sym="$2" out
    [ -n "$_NM_DUMP" ] || _NM_DUMP="$(swd_nm -n "$elf")"
    out="$(printf '%s\n' "$_NM_DUMP" | awk -v s="$sym" '$3 == s {print $1}')"
    [ "$(printf '%s\n' "$out" | grep -c .)" -eq 1 ] || {
        echo "expected exactly one '$sym' in $elf, found: ${out:-none}" >&2
        return 1
    }
    printf '0x%s\n' "$out"
}

# Read COUNT 32-bit words at ADDR, printing them as lowercase hex, space
# separated, in address order.
#
# probe-rs occasionally drops a transaction ("Target device did not respond")
# while the target is mid-reset or a core is halting the debug port; the read
# then comes back empty. An empty result parsed as zero would fail a judge
# against a live counter, so retry until COUNT words arrive or the attempts run
# out -- the same resilience the PowerShell runner got from re-invoking probe-rs.
swd_read() {
    local addr="$1" count="$2" words attempt
    for attempt in 1 2 3 4 5; do
        # Redirect probe-rs to a file rather than capturing with $(...): under
        # `make` on Windows, a native exe writing into a command-substitution
        # pipe intermittently yields nothing ("pipe is being closed"), which a
        # capture would read as an empty result. A file redirect is not a pipe
        # and is reliable; MSYS tools then parse the file. probe-rs prints
        # "<addr>: w0 w1 ..." over one or more lines -- drop the label, keep the
        # hex words.
        #
        # Do NOT pre-truncate the file (`: > "$_SWD_TMP"`): on Windows under
        # make, that bash-opened handle is not released before probe-rs opens
        # its own redirect, and probe-rs's write is then lost. Its `>` truncates
        # on its own, so the pre-clear was redundant as well as harmful.
        timeout 15 probe-rs read --chip RP2040 --probe "$PROBE" b32 "$addr" "$count" \
            > "$_SWD_TMP" 2>/dev/null || true
        # Parse with a single awk reading the file directly, not a
        # sed|tr|grep|tr chain: that cascade raises "tr: write error" under make
        # when grep finds nothing and closes the pipe early (SIGPIPE), which
        # pipefail turns fatal. Skip the "<addr>:" label (any field ending in a
        # colon) rather than stripping the colon and keeping it -- the address is
        # also eight hex digits and would be miscounted as a data word.
        words="$(awk '{for(i=1;i<=NF;i++){if($i ~ /:$/)continue; if($i~/^[0-9a-fA-F]{8}$/)printf "%s ",tolower($i)}}' "$_SWD_TMP" 2>/dev/null)"
        words="${words% }"
        if [ -n "$words" ] && [ "$(set -- $words; echo $#)" -eq "$count" ]; then
            printf '%s\n' "$words"
            return 0
        fi
        sleep 0.2
    done
    echo "swd_read: could not read $count words at $addr after 5 attempts" >&2
    return 1
}

# Opening the CMSIS-DAP probe occasionally fails transiently -- the previous
# probe-rs invocation has not fully released the USB device yet, or the target
# is mid-reset. These wrappers retry a few times and, only on final failure,
# surface the captured error rather than swallowing it into set -e.
swd_write() {
    local addr="$1" value="$2" attempt err
    for attempt in 1 2 3; do
        err="$(timeout 15 probe-rs write --chip RP2040 --probe "$PROBE" b32 "$addr" "$value" 2>&1 >/dev/null)" && return 0
        sleep 0.5
    done
    echo "swd_write: could not write $value to $addr: $err" >&2
    return 1
}

swd_download() {
    local elf="$1" attempt
    # A freshly released probe (previous probe-rs exiting, or a killed one) can
    # take a couple of seconds to become openable again, so back off between
    # attempts rather than hammering it.
    for attempt in 1 2 3 4 5 6; do
        timeout 90 probe-rs download --chip RP2040 --probe "$PROBE" --protocol swd \
            --speed "$SWD_SPEED" --non-interactive --preverify --verify --reset "$elf" && return 0
        echo "swd_download: attempt $attempt failed, retrying ..." >&2
        sleep 2
    done
    return 1
}

swd_reset() {
    timeout 20 probe-rs reset --chip RP2040 --probe "$PROBE" --protocol swd --speed "$SWD_SPEED"
}

# Decimal value of the Nth (0-based) word from a "w0 w1 ..." hex string.
word() {
    local words="$1" n="$2" w
    w="$(printf '%s\n' "$words" | tr ' ' '\n' | sed -n "$((n + 1))p")"
    printf '%d\n' "$((16#${w:-0}))"
}

# A nonzero 31-bit nonce. The firmware rejects zero, and the handshake must use
# a value the previous run could not have left behind.
fresh_nonce() {
    local n=0
    while [ "$n" -eq 0 ]; do
        n=$(( (RANDOM << 16 | RANDOM) & 0x7fffffff ))
    done
    printf '0x%08x\n' "$n"
}

abs_diff() { local a="$1" b="$2"; [ "$a" -ge "$b" ] && echo $((a - b)) || echo $((b - a)); }

# Echo a path in the form a native (non-MSYS) tool can open. git-bash normally
# auto-converts POSIX paths in exec arguments, but `make` runs with that
# conversion disabled, so a bare /tmp/... reaches native python or probe-rs
# unusable. cygpath rewrites it to the Windows path (same file); a no-op where
# cygpath is absent. Use it for any file path handed to a native exe.
win_path() {
    if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s\n' "$1"; fi
}
