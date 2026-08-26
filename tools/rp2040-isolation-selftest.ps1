# SPDX-License-Identifier: Apache-2.0
$ErrorActionPreference='Stop'
. "$PSScriptRoot/rp2040-isolation-common.ps1"
$words=[uint32[]]@(0x1390600d,42,24,3,800,2,200,200)
$console='[FLINT] MPU faults=24 rejected=3 iterations=800 cores=2'
Assert-Rp2040IsolationResult $words 42 $console
$tests=1
foreach ($change in @(@(0,0),@(1,43),@(2,23),@(3,2),@(4,799),@(5,1),@(6,199),@(7,199))) {
    $bad=$words.Clone(); $bad[$change[0]]=[uint32]$change[1]
    $rejected=$false
    try { Assert-Rp2040IsolationResult $bad 42 $console } catch { $rejected=$true }
    if (-not $rejected) { throw "bad isolation field $($change[0]) was accepted" }
    $tests++
}
foreach ($badConsole in @('', 'MPU faults=24 rejected=3 iterations=800 cores=2', '[FLINT] MPU faults=20 rejected=3 iterations=800 cores=2')) {
    $rejected=$false
    try { Assert-Rp2040IsolationResult $words 42 $badConsole } catch { $rejected=$true }
    if (-not $rejected) { throw 'wrong/missing UART evidence accepted' }
    $tests++
}
$rejected=$false
try { Assert-Rp2040IsolationResult ([uint32[]]@(0x1390600d,42)) 42 $console } catch { $rejected=$true }
if (-not $rejected) { throw 'truncated isolation result accepted' }
$tests++
$rejected=$false
try { Assert-Rp2040IsolationResult $words 0 $console } catch { $rejected=$true }
if (-not $rejected) { throw 'zero nonce accepted' }
$tests++
$fault=[uint32[]]@(0x139fa017,3,0,0x20001000,0x1000100c,0x21000000,4294967293L,0x10001000,0x20000000,0x20002000,42,0)
$faultConsole='PREVIOUS BOOT PANICKED unprivileged HardFault task=3 pc=1000100c'
Assert-Rp2040IsolationFault $fault 0x139f0001 0 42 0x1000100c $faultConsole
$tests++
foreach ($index in @(0,1,2,4,6,10)) {
    $bad=$fault.Clone(); $bad[$index]++
    $rejected=$false
    try { Assert-Rp2040IsolationFault $bad 0x139f0001 0 42 0x1000100c $faultConsole } catch { $rejected=$true }
    if (-not $rejected) { throw "bad fault field $index accepted" }
    $tests++
}
foreach ($case in @(0,1,2,3)) {
    $rejected=$false
    try {
        switch ($case) {
            0 { Assert-Rp2040IsolationFault $fault 0 0 42 0x1000100c $faultConsole }
            1 { Assert-Rp2040IsolationFault $fault 0x139f0001 42 42 0x1000100c $faultConsole }
            2 { Assert-Rp2040IsolationFault $fault 0x139f0001 0 0 0x1000100c $faultConsole }
            3 { Assert-Rp2040IsolationFault $fault 0x139f0001 0 42 0x1000100c '' }
        }
    } catch { $rejected=$true }
    if (-not $rejected) { throw 'incomplete fault/reset evidence accepted' }
    $tests++
}
$fault[2]=1
Assert-Rp2040IsolationFault $fault 0x139f0001 0 42 0x1000100c $faultConsole 1
$tests++
$rejected=$false
try { Assert-Rp2040IsolationFault $fault 0x139f0001 0 42 0x1000100c $faultConsole 0 } catch { $rejected=$true }
if (-not $rejected) { throw 'fault from wrong core accepted' }
$tests++
Write-Output "PASS: $tests isolation coverage, fault, nonce and UART fixtures"
