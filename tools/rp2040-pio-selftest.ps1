# SPDX-License-Identifier: Apache-2.0
$ErrorActionPreference='Stop'
. "$PSScriptRoot/rp2040-pio-common.ps1"
$words=[uint32[]]@(0x1750600d,42,2000,2,2,8,2,2,2,12000000)
$console="[FLINT] PIO words=2000 blocks=2 timeout=2 contention=8`n[FLINT] PIO fifo-full=2 fifo-empty=2 reopen=2"
Assert-Rp2040PioResult $words 42 12000000 $console
$tests=1
for ($i=0;$i -lt 10;$i++) {
    $bad=$words.Clone(); $bad[$i]=0
    $rejected=$false
    try { Assert-Rp2040PioResult $bad 42 12000000 $console } catch { $rejected=$true }
    if (-not $rejected) { throw "invalid PIO field $i accepted" }
    $tests++
}
foreach ($case in @(@{words=$words;nonce=43;console=$console},@{words=$words;nonce=0;console=$console},@{words=$words;nonce=42;console=''},@{words=$words;nonce=42;console=($console -split "`n")[0]},@{words=[uint32[]]@(0x1750600d,42);nonce=42;console=$console})) {
    $rejected=$false
    try { Assert-Rp2040PioResult $case.words $case.nonce 12000000 $case.console } catch { $rejected=$true }
    if (-not $rejected) { throw 'stale/truncated PIO result accepted' }
    $tests++
}
$words[9]=125000000; Assert-Rp2040PioResult $words 42 125000000 $console; $tests++
Write-Output "PASS: $tests PIO count, stale-result, profile and UART fixtures"
