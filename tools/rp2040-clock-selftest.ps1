# SPDX-License-Identifier: Apache-2.0
$ErrorActionPreference='Stop'
. "$PSScriptRoot/rp2040-clock-common.ps1"
$words=[uint32[]]@(0x1740600d,42,12000000,12000000,11998000,12002000,32,32,4,100000,100)
$console='[FLINT] cpu_hz=12000000 (measured against crystal-backed reference)'
Assert-Rp2040ClockResult $words 42 12000000 $console
$tests=1
foreach ($change in @(@(0,0),@(1,43),@(2,125000000),@(3,0),@(4,11000000),@(5,13000000),@(6,31),@(7,31),@(9,120001),@(10,89))) {
    $bad=$words.Clone(); $bad[$change[0]]=[uint32]$change[1]
    $rejected=$false
    try { Assert-Rp2040ClockResult $bad 42 12000000 $console } catch { $rejected=$true }
    if (-not $rejected) { throw "bad clock field $($change[0]) was accepted" }
    $tests++
}
foreach ($badConsole in @('', '[FLINT] cpu_hz=12000000 (ASSUMED: unavailable)', '[FLINT] cpu_hz=125000000 (measured against crystal-backed reference)')) {
    $rejected=$false
    try { Assert-Rp2040ClockResult $words 42 12000000 $badConsole } catch { $rejected=$true }
    if (-not $rejected) { throw 'unmeasured/wrong boot report accepted' }
    $tests++
}
$fast=[uint32[]]@(0x1740600d,42,125000000,125000000,124998000,125002000,32,32,0,100000,100)
Assert-Rp2040ClockResult $fast 42 125000000 '[FLINT] cpu_hz=125000000 (measured against crystal-backed reference)'
$tests++
foreach ($bad in @([uint32[]]@(0x1740600d,42), [uint32[]]@(0x1740600d,42,12000000,12000000,12003000,12002000,32,32,4,100000,100))) {
    $rejected=$false
    try { Assert-Rp2040ClockResult $bad 42 12000000 $console } catch { $rejected=$true }
    if (-not $rejected) { throw 'truncated or reversed-range clock result accepted' }
    $tests++
}
Write-Output "PASS: $tests clock result, stale-nonce, frequency and boot-provenance fixtures"
