# SPDX-License-Identifier: Apache-2.0
# Tuple-checked RP2040 ELF/BIN/UF2 production and flashing.

[CmdletBinding()]
param(
    [ValidateSet('verify-elf', 'verify-uf2', 'convert', 'flash', 'swd')]
    [string]$Action,
    [string]$Architecture,
    [string]$Soc,
    [string]$Board,
    [string]$Elf,
    [string]$Uf2,
    [string]$Bin,
    [string]$Objcopy = 'arm-none-eabi-objcopy',
    [string]$Objdump = 'arm-none-eabi-objdump',
    [string]$Elf2Uf2 = 'elf2uf2-rs',
    [string]$ProbeRs = 'probe-rs',
    [string]$ProbeSerial,
    [int]$TimeoutSeconds = 20
)

$ErrorActionPreference = 'Stop'
if ($Architecture -ne 'armv6m' -or $Soc -ne 'rp2040' -or
    $Board -notin @('wio-rp2040-mini', 'raspberry-pi-pico')) {
    throw "unsupported build tuple: expected armv6m/rp2040 with a supported board; got $Architecture/$Soc/$Board"
}

function Invoke-Checked {
    param([string]$Program, [string[]]$Arguments)
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit $LASTEXITCODE" }
}

function Invoke-SwdAfterResetPulse {
    param([string[]]$Arguments)

    $selector = if ($ProbeSerial) { @('--probe', "2e8a:000c:$ProbeSerial") } else { @('--non-interactive') }
    $pulse = @(
        'reset', '--chip', 'RP2040', '--protocol', 'swd', '--speed', '100',
        '--connect-under-reset'
    ) + $selector

    # RP2040 does not answer SWD while RUN is held low. probe-rs therefore
    # reports that attach as failed, then releases RUN while closing the probe.
    # The following operation must start immediately, before target firmware
    # can disturb the debug port.
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        & $ProbeRs @pulse 2>&1 | Out-Null
        & $ProbeRs @Arguments
        if ($LASTEXITCODE -eq 0) { return }
    }
    throw "SWD operation failed after three reset-release-attach attempts"
}

function Test-Elf {
    if (-not $Elf) { throw '-Elf is required' }
    $resolved = (Resolve-Path -LiteralPath $Elf).Path
    $header = & $Objdump -f $resolved 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0 -or $header -notmatch 'arm') { throw 'input is not an ARM ELF' }
    $sections = & $Objdump -h $resolved 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { throw 'could not read ELF sections' }
    if ($sections -notmatch '(?m)^\s*\d+\s+\.boot2\s+00000100\s+10000000\s+') { throw '.boot2 is not exactly 256 bytes at 0x10000000' }
    if ($sections -notmatch '(?m)^\s*\d+\s+\.vector_table\s+[0-9a-fA-F]+\s+10000100\s+') { throw '.vector_table is not at 0x10000100' }
    return $resolved
}

function Test-Uf2 {
    if (-not $Uf2) { throw '-Uf2 is required' }
    $resolved = (Resolve-Path -LiteralPath $Uf2).Path
    $bytes = [IO.File]::ReadAllBytes($resolved)
    if ($bytes.Length -eq 0 -or $bytes.Length % 512 -ne 0) { throw 'UF2 length is not a nonempty sequence of 512-byte blocks' }
    for ($offset=0; $offset -lt $bytes.Length; $offset += 512) {
        if ([BitConverter]::ToUInt32($bytes,$offset) -ne 0x0A324655u -or [BitConverter]::ToUInt32($bytes,$offset+4) -ne 0x9E5D5157u -or [BitConverter]::ToUInt32($bytes,$offset+508) -ne 0x0AB16F30u) { throw "invalid UF2 magic at block $($offset/512)" }
        $flags = [BitConverter]::ToUInt32($bytes,$offset+8)
        $family = [BitConverter]::ToUInt32($bytes,$offset+28)
        if (($flags -band 0x2000) -eq 0 -or $family -ne 0xE48BFF56u) { throw 'UF2 is not tagged for the RP2040 family' }
    }
    return $resolved
}

if ($Action -eq 'verify-elf') { [void](Test-Elf); Write-Host 'PASS: ARM ELF has RP2040 boot2 and vector placement.'; exit 0 }
if ($Action -eq 'verify-uf2') { [void](Test-Uf2); Write-Host 'PASS: UF2 is tagged for RP2040.'; exit 0 }

if ($Action -eq 'convert') {
    $resolvedElf = Test-Elf
    if (-not $Bin) { $Bin = [IO.Path]::ChangeExtension($resolvedElf, '.bin') }
    if (-not $Uf2) { $Uf2 = [IO.Path]::ChangeExtension($resolvedElf, '.uf2') }
    Invoke-Checked $Objcopy @('-O','binary',$resolvedElf,$Bin)
    Invoke-Checked $Elf2Uf2 @($resolvedElf,$Uf2)
    [void](Test-Uf2)
    [pscustomobject]@{ state='converted'; elf=$resolvedElf; bin=(Resolve-Path $Bin).Path; uf2=(Resolve-Path $Uf2).Path } | ConvertTo-Json -Compress
    exit 0
}

if ($Action -eq 'flash') {
    $resolvedUf2 = Test-Uf2
    & (Join-Path $PSScriptRoot 'rp2040-target.ps1') -Action bootsel-flash-reconnect -Uf2Path $resolvedUf2 -TimeoutSeconds $TimeoutSeconds
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    exit 0
}

if ($Action -eq 'swd') {
    $resolvedElf = Test-Elf
    $selector = if ($ProbeSerial) { @('--probe', "2e8a:000c:$ProbeSerial") } else { @('--non-interactive') }
    Invoke-SwdAfterResetPulse (@(
        'download', '--chip', 'RP2040', '--protocol', 'swd', '--speed', '100'
    ) + $selector + @($resolvedElf))
    exit 0
}
