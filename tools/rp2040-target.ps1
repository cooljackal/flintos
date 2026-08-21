# SPDX-License-Identifier: Apache-2.0
# Discover an RP2040 USB serial port and observe a bounded reconnect cycle.

[CmdletBinding()]
param(
    [ValidateSet('discover', 'probe', 'observe-reconnect')]
    [string]$Action = 'discover',
    [Alias('Vid')]
    [string]$VendorId = '2E8A',
    [Alias('Pid')]
    [string]$ProductId = '000A',
    [string]$Serial,
    [ValidateRange(1, 300)]
    [int]$TimeoutSeconds = 10,
    [ValidateRange(10, 5000)]
    [int]$PollMilliseconds = 100,
    [string]$Fixture
)

$ErrorActionPreference = 'Stop'

function ConvertTo-Device {
    param([object]$Item)
    $port = if ($Item.port) { [string]$Item.port } elseif ($Item.Name -match '\((COM[0-9]+)\)') { $Matches[1] } else { $null }
    $id = if ($Item.instance_id) { [string]$Item.instance_id } else { [string]$Item.PNPDeviceID }
    $deviceSerial = if ($Item.serial) { [string]$Item.serial } else {
        $parts = $id -split '\\'
        if ($parts.Count -ge 3) { $parts[-1] } else { '' }
    }
    [pscustomobject]@{ port = $port; instance_id = $id; serial = $deviceSerial }
}

function Get-LiveDevices {
    Get-CimInstance Win32_PnPEntity | Where-Object {
        $_.PNPDeviceID -match "^USB\\VID_$VendorId&PID_$ProductId" -and $_.Name -match '\(COM[0-9]+\)'
    } | ForEach-Object { ConvertTo-Device $_ }
}

$fixtureData = if ($Fixture) { Get-Content -Raw -LiteralPath $Fixture | ConvertFrom-Json } else { $null }
$script:snapshotIndex = 0
function Get-Snapshot {
    if (-not $Fixture) { return @(Get-LiveDevices) }
    if ($fixtureData -is [array] -or -not $fixtureData.PSObject.Properties['snapshots']) {
        return @($fixtureData | ForEach-Object { ConvertTo-Device $_ })
    }
    $snapshots = @($fixtureData.snapshots)
    $index = [Math]::Min($script:snapshotIndex, $snapshots.Count - 1)
    $script:snapshotIndex++
    return @($snapshots[$index] | ForEach-Object { ConvertTo-Device $_ })
}

function Select-Device {
    param([object[]]$Devices, [switch]$AllowMissing)
    $matches = @($Devices | Where-Object {
        $_.port -and (-not $Serial -or $_.serial -eq $Serial)
    } | Sort-Object instance_id, port)
    if ($matches.Count -eq 0) {
        if ($AllowMissing) { return $null }
        throw "no USB serial device matched VID=$VendorId PID=$ProductId$(if ($Serial) { " serial=$Serial" })"
    }
    if ($matches.Count -gt 1) {
        $choices = ($matches | ForEach-Object { "$($_.port) [$($_.serial)]" }) -join ', '
        throw "multiple USB serial devices matched; select one with -Serial: $choices"
    }
    return $matches[0]
}

function Write-Device {
    param([object]$Device, [string]$State)
    [pscustomobject]@{
        state = $State
        port = $Device.port
        vid = $VendorId.ToUpperInvariant()
        pid = $ProductId.ToUpperInvariant()
        serial = $Device.serial
        instance_id = $Device.instance_id
    } | ConvertTo-Json -Compress
}

$initial = Select-Device (Get-Snapshot)
if ($Action -eq 'discover') {
    Write-Device $initial 'connected'
    exit 0
}

if ($Action -eq 'probe') {
    if ($Fixture) {
        Write-Device $initial 'transport-opened'
        exit 0
    }
    $port = [System.IO.Ports.SerialPort]::new($initial.port, 115200)
    $port.ReadTimeout = $TimeoutSeconds * 1000
    $port.WriteTimeout = $TimeoutSeconds * 1000
    try { $port.Open() } finally { $port.Dispose() }
    Write-Device $initial 'transport-opened'
    exit 0
}

$deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
$disconnected = $false
while ([DateTime]::UtcNow -lt $deadline) {
    $current = Select-Device (Get-Snapshot) -AllowMissing
    if (-not $current) {
        $disconnected = $true
    } elseif ($disconnected) {
        if ($current.serial -ne $initial.serial) {
            throw "a different matching device appeared after disconnect: expected serial '$($initial.serial)', got '$($current.serial)'"
        }
        Write-Device $current 'reconnected'
        exit 0
    }
    if (-not $Fixture) { Start-Sleep -Milliseconds $PollMilliseconds }
}
if ($disconnected) { throw "device disconnected but did not reconnect within $TimeoutSeconds seconds" }
throw "no USB disconnect was observed within $TimeoutSeconds seconds; reset is not proven"
