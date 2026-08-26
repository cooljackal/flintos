# SPDX-License-Identifier: Apache-2.0
# Pure acceptance judge; the counter's reference is nominal, not calibrated.
function Assert-Rp2040ClockResult {
    param([uint32[]]$Words, [uint32]$Nonce, [uint32]$ExpectedHz, [string]$Console)
    if ($Words.Count -ne 11 -or $Words[0] -ne 0x1740600d -or $Nonce -eq 0 -or $Words[1] -ne $Nonce) {
        throw 'clock result is incomplete, failed, or stale'
    }
    if ($ExpectedHz -notin @(12000000,125000000) -or $Words[2] -ne $ExpectedHz -or
        [Math]::Abs([long]$Words[3]-$ExpectedHz) -gt 5000 -or
        [Math]::Abs([long]$Words[4]-$ExpectedHz) -gt 5000 -or
        [Math]::Abs([long]$Words[5]-$ExpectedHz) -gt 5000 -or $Words[4] -gt $Words[5] -or
        $Words[6] -ne 32 -or $Words[7] -ne 32 -or
        $Words[9] -lt 90000 -or $Words[9] -gt 120000 -or $Words[10] -lt 90 -or $Words[10] -gt 120) {
        throw 'clock frequency, per-core samples, or tick timing failed acceptance'
    }
    $bootLine="[FLINT] cpu_hz=$($Words[3]) (measured against crystal-backed reference)"
    if (-not $Console.Contains($bootLine) -or $Console.Contains('ASSUMED:')) {
        throw 'boot did not explicitly report the measured clock over UART'
    }
}
