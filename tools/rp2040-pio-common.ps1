# SPDX-License-Identifier: Apache-2.0
function Assert-Rp2040PioResult {
    param([uint32[]]$Words,[uint32]$Nonce,[uint32]$ExpectedHz,[string]$Console)
    if ($Words.Count -ne 10 -or $Words[0] -ne 0x1750600d -or $Nonce -eq 0 -or $Words[1] -ne $Nonce) {
        throw 'PIO result failed, incomplete, or stale'
    }
    $expected=@(2000,2,2,8,2,2,2)
    for ($i=0;$i -lt $expected.Count;$i++) {
        if ($Words[$i+2] -ne $expected[$i]) { throw "PIO result count $($i+2) failed" }
    }
    if ($ExpectedHz -notin @(12000000,125000000) -or [Math]::Abs([long]$Words[9]-$ExpectedHz) -gt 5000) { throw 'PIO clock profile mismatch' }
    foreach ($line in @('[FLINT] PIO words=2000 blocks=2 timeout=2 contention=8','[FLINT] PIO fifo-full=2 fifo-empty=2 reopen=2')) {
        if (-not $Console.Contains($line)) { throw 'PIO UART evidence missing' }
    }
}
