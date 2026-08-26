#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Run one of the RP2040 base self-test suites over SWD and judge its retained
# result. Cross-platform port of the SWD paths of rp2040-run-selftest.ps1.
#
# Every suite here follows the same contract: the image clears
# FLINT_RP2040_TEST_STATUS, runs, and publishes 0x0000600d there on success
# (0x000001xx on failure). Measurement counters are retained in named symbols
# and read back over SWD; `dma` and `io` additionally assert UART markers.
#
# The BOOTSEL-only paths of the PowerShell runner (bare acceptance, watchdog,
# diagnostics, UF2 cycling) are not ported: they need the target's own USB,
# which this SWD-only rig does not have.
#
# Usage: rp2040-run-selftest.sh <elf> <suite> [serial_port]
#   suite: mutex | race | pwm | adc-entropy | dma | io | bus

set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/rp2040-swd-lib.sh"

ELF="${1:?usage: $0 <elf> <suite> [serial_port]}"
SUITE="${2:?suite: mutex|race|pwm|adc-entropy|dma|io|bus}"
PORT="${3:-${FLINT_UART_PORT:-}}"
PY="${FLINT_PY:-python}"
TIMEOUT="${FLINT_TIMEOUT:-30}"

STATUS="$(swd_addr "$ELF" FLINT_RP2040_TEST_STATUS)"

CONSOLE="$(win_path "$(mktemp)")"; CAP_PID=""
cleanup() { if [ -n "$CAP_PID" ]; then kill "$CAP_PID" 2>/dev/null || true; wait "$CAP_PID" 2>/dev/null || true; fi; rm -f "$CONSOLE" "${_SWD_TMP:-}"; }
trap cleanup EXIT

needs_uart=0
case "$SUITE" in dma|io) needs_uart=1 ;; esac
if [ "$needs_uart" -eq 1 ]; then
    [ -n "$PORT" ] || { echo "FAIL: suite '$SUITE' needs a serial port (UART markers)" >&2; exit 2; }
    "$PY" "$(win_path "$HERE/rp2040-serial-capture.py")" --port "$PORT" --out "$CONSOLE" --seconds $((TIMEOUT + 10)) &
    CAP_PID=$!
    sleep 0.5
fi

# Clear the status word, then flash. A stale 0x600d from a prior run would
# otherwise read as an instant pass.
swd_write "$STATUS" 0
echo "downloading $ELF over SWD (suite=$SUITE) ..."
swd_download "$ELF"

# Poll for the pass sentinel. 0x0000600d = pass; 0x000001xx = an in-image
# failure code, which we surface rather than wait out.
PASS=0
for _ in $(seq 1 $((TIMEOUT * 10))); do
    s="$(word "$(swd_read "$STATUS" 1)" 0)"
    [ "$s" -eq $((0x0000600d)) ] && { PASS=1; break; }
    [ $((s & 0xffffff00)) -eq $((0x00000100)) ] && break
    sleep 0.1
done
[ "$PASS" -eq 1 ] || { echo "FAIL: suite '$SUITE' did not publish a passing status (status=$(printf 0x%08x "$s"))" >&2; exit 1; }

# Read one retained counter symbol as a decimal.
rd() { word "$(swd_read "$(swd_addr "$ELF" "$1")" 1)" 0; }
fail() { echo "FAIL: $1" >&2; exit 1; }
inrange() { [ "$1" -ge "$2" ] && [ "$1" -le "$3" ]; }
# Wait for a UART marker rather than grepping once: a line can be emitted a few
# seconds after the SWD status word flips to pass (the DMA marker trails it), and
# the serial capture is still writing to $CONSOLE in the background.
umark() {
    local m="$1" i
    for i in $(seq 1 40); do
        grep -qF "$m" "$CONSOLE" && return 0
        sleep 0.2
    done
    fail "UART output missing: $m"
}

case "$SUITE" in
mutex)
    p="$(rd MUTEX_SOAK_PROGRESS)"
    [ "$p" -eq 2000 ] || fail "mutex cycles $p != 2000"
    echo "PASS: mutex  priority-inheritance 2 cores, cycles=$p"
    ;;
race)
    h="$(rd RACE_ISR_HANDLED)"; s="$(rd RACE_ISR_SENT)"; r="$(rd RACE_TASK_RECEIVED)"; n="$(rd RACE_NESTED_MASKED)"
    [ "$h" -eq 10000 ] || fail "RACE_ISR_HANDLED $h != 10000"
    [ "$s" -eq 10000 ] || fail "RACE_ISR_SENT $s != 10000"
    [ "$r" -eq 10000 ] || fail "RACE_TASK_RECEIVED $r != 10000"
    [ "$n" -eq 2500 ]  || fail "RACE_NESTED_MASKED $n != 2500"
    echo "PASS: race  isr_handled=$h isr_sent=$s task_received=$r nested_masked=$n"
    ;;
pwm)
    e="$(rd PWM_EDGE_COUNT)"; per="$(rd PWM_PERIOD_US)"; hi="$(rd PWM_HIGH_US)"
    [ "$e" -eq 2000 ] || fail "PWM_EDGE_COUNT $e != 2000"
    inrange "$per" 950 1050 || fail "PWM_PERIOD_US $per out of [950,1050]"
    inrange "$hi" 400 600   || fail "PWM_HIGH_US $hi out of [400,600]"
    echo "PASS: pwm  edges=$e period_us=$per high_us=$hi"
    ;;
adc-entropy)
    sc="$(rd ADC_SAMPLE_COUNT)"; mn="$(rd ADC_MIN_RAW)"; mx="$(rd ADC_MAX_RAW)"; av="$(rd ADC_AVG_RAW)"
    tu="$(rd ADC_TEMP_MILLI_C)"; t="$tu"; [ "$t" -ge 2147483648 ] && t=$((t - 4294967296))
    eb="$(rd ENTROPY_RAW_BITS)"; eo="$(rd ENTROPY_RAW_ONES)"; et="$(rd ENTROPY_TRANSITIONS)"; ec="$(rd ENTROPY_CHECKSUM)"
    [ "$sc" -eq 1024 ] || fail "ADC_SAMPLE_COUNT $sc != 1024"
    [ "$mn" -ne 0 ] || fail "ADC_MIN_RAW is 0"
    [ "$mx" -lt 4095 ] || fail "ADC_MAX_RAW $mx >= 4095"
    [ "$mn" -lt "$mx" ] || fail "ADC_MIN_RAW $mn >= ADC_MAX_RAW $mx"
    inrange "$t" -40000 125000 || fail "ADC temperature ${t}m°C out of [-40000,125000]"
    [ "$eb" -eq 4096 ] || fail "ENTROPY_RAW_BITS $eb != 4096"
    inrange "$eo" 1024 3072 || fail "ENTROPY_RAW_ONES $eo out of [1024,3072]"
    inrange "$et" 819 3276  || fail "ENTROPY_TRANSITIONS $et out of [819,3276]"
    [ "$ec" -ne 0 ] || fail "ENTROPY_CHECKSUM is 0"
    echo "PASS: adc-entropy  adc[min=$mn max=$mx avg=$av temp=${t}m°C n=$sc]  entropy[ones=$eo/$eb transitions=$et]"
    ;;
dma)
    sleep 0.3; umark "ARM DMA PASS rounds=100 bytes=512 timeout=ok"
    echo "PASS: dma  timeout-recovery + 100x512-byte UART loopback"
    ;;
io)
    sleep 0.3
    umark "ARM UART LOOPBACK payloads=1000 bytes=16000"
    umark "ARM GPIO LOOPBACK edges=10000"
    echo "PASS: io  uart 1000 payloads + gpio 10000 exact edges"
    ;;
bus)
    sb="$(rd BUS_SPI_BYTES)"; sc="$(rd BUS_SPI_CHECKSUM)"; it="$(rd BUS_I2C_TRANSACTIONS)"; ib="$(rd BUS_I2C_BYTES)"
    inr="$(rd BUS_I2C_NACK_RECOVERED)"; ms="$(rd BUS_MASTER_STAGE)"; ss="$(rd BUS_SLAVE_STAGE)"
    st="$(rd BUS_SPI_TIMEOUT_US)"; itu="$(rd BUS_I2C_TIMEOUT_US)"
    [ "$sb" -eq 4096 ] || fail "BUS_SPI_BYTES $sb != 4096"
    [ "$sc" -ne 0 ] || fail "BUS_SPI_CHECKSUM is 0"
    [ "$it" -eq 1001 ] || fail "BUS_I2C_TRANSACTIONS $it != 1001"
    [ "$ib" -eq 8008 ] || fail "BUS_I2C_BYTES $ib != 8008"
    [ "$inr" -eq 1 ] || fail "BUS_I2C_NACK_RECOVERED $inr != 1"
    [ "$ms" -eq 4 ] || fail "BUS_MASTER_STAGE $ms != 4"
    [ "$ss" -eq 4 ] || fail "BUS_SLAVE_STAGE $ss != 4"
    inrange "$st" 50000 99999 || fail "BUS_SPI_TIMEOUT_US $st out of [50000,100000)"
    inrange "$itu" 50000 99999 || fail "BUS_I2C_TIMEOUT_US $itu out of [50000,100000)"
    echo "PASS: bus  spi_bytes=$sb i2c_tx=$it i2c_bytes=$ib nack_recovered=$inr"
    ;;
*)
    echo "unknown suite '$SUITE'" >&2; exit 2 ;;
esac
