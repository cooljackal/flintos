# SPDX-License-Identifier: Apache-2.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ElfPath,
    [Parameter(Mandatory)][string]$Uf2Path,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{8}$')][string]$ImageId,
    [ValidatePattern('^[0-9a-fA-F]{8}$')][string]$InitialImageId,
    [Parameter(Mandatory)][string]$ProbeSerial,
    [Parameter(Mandatory)][string]$BootselSerial,
    [Parameter(Mandatory)][string]$LocationPath,
    [ValidateRange(1,1000)][int]$Cycles=1,
    [switch]$SkipInitialDownload,
    [switch]$FaultTests,
    [string]$ControlTool='target/x86_64-pc-windows-msvc/debug/usb-control.exe'
)
$ErrorActionPreference='Stop'
. "$PSScriptRoot/rp2040-usb-common.ps1"
$elf=(Resolve-Path -LiteralPath $ElfPath).Path
$uf2=(Resolve-Path -LiteralPath $Uf2Path).Path
$control=(Resolve-Path -LiteralPath $ControlTool).Path
$probe="2e8a:000c:$ProbeSerial"
if ($InitialImageId -and -not $SkipInitialDownload) { throw '-InitialImageId requires -SkipInitialDownload' }
# Reuse the existing tuple/family/range validation before any mutation.
& "$PSScriptRoot/rp2040-image.ps1" -Action verify-uf2 -Architecture armv6m -Soc rp2040 -Board raspberry-pi-pico -Uf2 $uf2
$fixtureLock=Enter-UsbFixtureLock $LocationPath
try {

function Invoke-Download {
    Invoke-UsbBoundedProcess 'probe-rs' @('download','--chip','RP2040','--probe',$probe,'--protocol','swd','--non-interactive','--speed','100','--preverify','--verify','--reset',$elf) 60 | Out-Host
}
function Invoke-RomFlash {
    $rom=Wait-UsbFixtureDevice 'rom' $LocationPath $BootselSerial
    $volume=Get-UsbRomVolume $rom
    Invoke-UsbBoundedProcess 'pwsh' @('-NoProfile','-File',"$PSScriptRoot/rp2040-usb-copy.ps1",'-Source',$uf2,'-Destination',$volume) 30 | Out-Host
    Wait-UsbFixtureDevice 'app' $LocationPath $BootselSerial
}
function Test-App {
    param([object]$Device,[int[]]$Lengths,[string]$ExpectedImage=$ImageId)
    $serial=Open-UsbSerial $Device.port
    try { Test-UsbHello $serial $ExpectedImage; foreach ($length in $Lengths) { Test-UsbEcho $serial $length }; Test-UsbHello $serial $ExpectedImage }
    finally { $serial.Dispose() }
}

if (-not $SkipInitialDownload) { Invoke-Download }
$device=Wait-UsbFixtureDevice 'app' $LocationPath $BootselSerial
Invoke-UsbBoundedProcess $control @('check',$LocationPath) 15 | Out-Host
Test-App $device @(1,7,63,64,65,127,128,129,255,256,257,511,512,513,4096,65536) $(if ($InitialImageId) { $InitialImageId } else { $ImageId })
for ($cycle=1;$cycle -le $Cycles;$cycle++) {
    $watch=[Diagnostics.Stopwatch]::StartNew()
    $result=Invoke-UsbRecoverableUpdate {
        Invoke-UsbBoundedProcess $control @('bootsel',$LocationPath) 10 | Out-Null
        # ROM enumeration is a recovery state, NEVER a test result.
        Invoke-RomFlash
    } {
        Invoke-Download
        Wait-UsbFixtureDevice 'app' $LocationPath $BootselSerial
    } { param($candidate) Test-App $candidate @(4096) }
    $device=$result.device
    [pscustomobject]@{cycle=$cycle;state=$(if ($result.recovered) {'swd-recovered-data-pass'} else {'usb-flash-data-pass'});image=$ImageId;echo_bytes=4096;elapsed_ms=$watch.ElapsedMilliseconds;swd_fallback=$result.recovered;manual_bootsel=$false} | ConvertTo-Json -Compress
}
if ($FaultTests) {
    foreach ($fault in @(2,3)) {
        $serial=Open-UsbSerial $device.port
        try {
            Test-UsbHello $serial $ImageId
            $nonce=[byte[]]::new(8); [Security.Cryptography.RandomNumberGenerator]::Fill($nonce)
            Send-UsbCommand $serial $fault $nonce
            Assert-UsbHello (Read-UsbExact $serial 20) $ImageId $nonce
        }
        finally { $serial.Dispose() }
        if ($fault -eq 2) {
            # Only the watchdog may produce ROM here. No reset/reload until the
            # target has independently entered the expected recovery device.
            $device=Invoke-RomFlash
        } else {
            # Tick/USB IRQ still run but the task does not. Demand a timeout,
            # not a stale PASS, then one bounded independent SWD recovery.
            $serial=Open-UsbSerial $device.port; $timedOut=$false
            try { Test-UsbHello $serial $ImageId }
            catch [TimeoutException] { $timedOut=$true }
            finally { $serial.Dispose() }
            if (-not $timedOut) { throw 'stalled task unexpectedly answered the fresh-run challenge' }
            $result=Invoke-UsbRecoverableUpdate {
                Invoke-UsbBoundedProcess $control @('bootsel',$LocationPath) 10 | Out-Null
                Invoke-RomFlash
            } {
                Invoke-Download
                Wait-UsbFixtureDevice 'app' $LocationPath $BootselSerial
            } { param($candidate) Test-App $candidate @(513) }
            if (-not $result.recovered) { throw 'stalled task did not exercise independent SWD recovery' }
            $device=$result.device
        }
        Test-App $device @(513)
        [pscustomobject]@{state='fault-recovery-pass';fault=$fault;transport=$(if ($fault -eq 2) {'watchdog-rom-usb'} else {'swd'});manual_bootsel=$false} | ConvertTo-Json -Compress
    }
}
Invoke-UsbBoundedProcess $control @('check',$LocationPath) 15 | Out-Host
Write-Output 'USB SUITE PASS'
} finally { $fixtureLock.ReleaseMutex(); $fixtureLock.Dispose() }
