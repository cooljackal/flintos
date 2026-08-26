# SPDX-License-Identifier: Apache-2.0
# Guard the generated XIP-off code against accidental flash calls/literal pools.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$ElfPath)
$ErrorActionPreference = 'Stop'
$elf = (Resolve-Path -LiteralPath $ElfPath).Path
$llvm = Join-Path ((& rustc --print sysroot).Trim()) 'lib/rustlib/x86_64-pc-windows-msvc/bin'
$nm = @(& "$llvm/llvm-nm.exe" -n $elf)
if ($LASTEXITCODE -ne 0) { throw 'cannot read ELF symbols' }
function Address([string]$Name) {
    $line = @($nm | Where-Object { $_ -match " $Name`$" })
    if ($line.Count -ne 1) { throw "missing $Name" }
    [Convert]::ToUInt32(($line[0] -split '\s+')[0], 16)
}
$start = Address '_sram_func_start'
$end = Address '_sram_func_end'
if ($start -lt 0x20000000 -or $end -gt 0x20042000 -or $start -ge $end) { throw 'RAM code outside SRAM' }
foreach ($function in @('execute_rom', 'service_request')) {
    $matches = @($nm | Where-Object { $_ -match "^[0-9a-f]+ [tT] _ZN.*$function" })
    if ($matches.Count -ne 1) { throw "missing or duplicated $function" }
    $address = [Convert]::ToUInt32(($matches[0] -split '\s+')[0], 16)
    if ($address -lt $start -or $address -ge $end) { throw "$function was placed in flash" }
}
$assembly = @(& "$llvm/llvm-objdump.exe" -d --section=.data "--start-address=$start" "--stop-address=$end" $elf)
if ($LASTEXITCODE -ne 0) { throw 'cannot disassemble RAM code' }
$bad = @($assembly | Where-Object { $_ -match '\.word\s+0x1[0-3][0-9a-fA-F]{6}\b|\b[bB][a-z]*\s+0x1[0-3][0-9a-fA-F]{6}\b' })
if ($bad.Count) { throw "XIP-off code has flash references: $($bad -join '; ')" }
# Indirect calls still require review: their RAM-resident function table is
# resolved against ROM bounds, and boot2 is explicitly copied to the stack.
[pscustomobject]@{state='passed'; ram_code_bytes=$end-$start; direct_flash_references=0} | ConvertTo-Json -Compress
