# SPDX-License-Identifier: Apache-2.0
function Assert-Rp2040IsolationResult {
    param([uint32[]]$Words,[uint32]$Nonce,[string]$Console)
    if ($Words.Count -ne 8 -or $Words[0] -ne 0x1390600d -or $Nonce -eq 0 -or $Words[1] -ne $Nonce) {
        throw 'isolation result failed, incomplete, or stale'
    }
    if ($Words[2] -ne 24 -or $Words[3] -ne 3 -or $Words[4] -ne 800 -or $Words[5] -ne 2 -or $Words[6] -lt 200 -or $Words[7] -lt 200) {
        throw 'isolation coverage incomplete'
    }
    if (-not $Console.Contains('[FLINT] MPU faults=24 rejected=3 iterations=800 cores=2')) {
        throw 'isolation UART evidence missing'
    }
}

function Assert-Rp2040IsolationFault {
    param([uint32[]]$Fault,[uint32]$State,[uint32]$AfterNonce,[uint32]$Nonce,[uint32]$Pc,[string]$Console,[ValidateSet(0,1)][int]$Core=0)
    if ($Fault.Count -ne 12 -or $State -ne 0x139f0001 -or $AfterNonce -ne 0 -or $Nonce -eq 0 -or
        $Fault[0] -ne 0x139fa017 -or $Fault[1] -ne 3 -or $Fault[2] -ne $Core -or
        $Fault[4] -ne $Pc -or $Fault[6] -ne 4294967293L -or $Fault[10] -ne $Nonce) {
        throw 'unexpected-fault record/reset evidence failed'
    }
    if (-not $Console.Contains('PREVIOUS BOOT PANICKED') -or -not $Console.Contains(('unprivileged HardFault task=3 pc={0:x8}' -f $Pc))) {
        throw 'unexpected-fault panic attribution missing'
    }
}
