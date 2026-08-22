# SPDX-License-Identifier: Apache-2.0
# Host-only checks for tuple and UF2 family protection.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$tool = Join-Path $PSScriptRoot 'rp2040-image.ps1'
$work = Join-Path $root 'target/tmp/rp2040-image-tests'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$failures = 0

function New-Uf2([string]$Path, [uint32]$Family) {
    $block = [byte[]]::new(512)
    foreach ($pair in @(@(0,0x0A324655u),@(4,0x9E5D5157u),@(8,0x2000u),@(28,$Family),@(508,0x0AB16F30u))) {
        [BitConverter]::GetBytes([uint32]$pair[1]).CopyTo($block, [int]$pair[0])
    }
    [IO.File]::WriteAllBytes($Path, $block)
}

function Invoke-Case([string]$Name, [string[]]$Arguments, [int]$Want, [string]$Pattern) {
    $output = & pwsh -NoProfile -File $tool @Arguments 2>&1 | Out-String
    if ($LASTEXITCODE -eq $Want -and $output -match $Pattern) { Write-Host "  ok    $Name"; return }
    Write-Host "  FAIL  $Name (exit $LASTEXITCODE, wanted $Want): $output"
    $script:failures++
}

$good = Join-Path $work 'rp2040.uf2'
$wrong = Join-Path $work 'wrong-family.uf2'
New-Uf2 $good 0xE48BFF56u
New-Uf2 $wrong 0x12345678u
$tuple = @('-Architecture','armv6m','-Soc','rp2040','-Board','wio-rp2040-mini')
Invoke-Case 'rp2040-family' (@('-Action','verify-uf2','-Uf2',$good) + $tuple) 0 'tagged for RP2040'
Invoke-Case 'wrong-family' (@('-Action','verify-uf2','-Uf2',$wrong) + $tuple) 1 'not tagged for the RP2040 family'
Invoke-Case 'wrong-tuple' @('-Action','verify-uf2','-Uf2',$good,'-Architecture','xtensa','-Soc','rp2040','-Board','wio-rp2040-mini') 1 'unsupported build tuple'

$linker = Get-Content -Raw (Join-Path $root 'arch/armv6m/rp2040.ld')
foreach ($required in @('SIZEOF(.boot2) == 0x100','ADDR(.vector_table) == 0x10000100','SIZEOF(.vector_table) >= 0xC0')) {
    if ($linker -notmatch [regex]::Escape($required)) { Write-Host "  FAIL  linker assertion: $required"; $failures++ }
}

Remove-Item -Recurse -Force $work
if ($failures) { throw "$failures RP2040 image self-test(s) failed" }
Write-Host 'All RP2040 image self-tests passed.'
