# SPDX-License-Identifier: Apache-2.0
# Discover an RP2040 USB serial port and observe a bounded reconnect cycle.

[CmdletBinding()]
param(
    [ValidateSet('discover', 'probe', 'observe-reconnect', 'reset-reconnect', 'bootsel-flash-reconnect', 'await-test-bootsel', 'judge-log')]
    [string]$Action = 'discover',
    [Alias('Vid')]
    [string]$VendorId = '2E8A',
    [Alias('Pid')]
    [string]$ProductId = '000A',
    [string]$Serial,
    [string]$BootselSerial,
    [string]$BootselLocationPath,
    [ValidateRange(1, 300)]
    [int]$TimeoutSeconds = 10,
    [ValidateRange(10, 5000)]
    [int]$PollMilliseconds = 100,
    [string]$Fixture,
    [string]$PicotoolPath = 'picotool',
    [string]$LogPath,
    [string]$Uf2Path
)

$ErrorActionPreference = 'Stop'

if ($Action -eq 'judge-log') {
    if (-not $LogPath) { throw '-LogPath is required for judge-log' }
    $lines = @(Get-Content -LiteralPath $LogPath | ForEach-Object { $_.TrimEnd("`r") })
    if (-not ($lines -contains '[FLINT] SELFTEST BEGIN')) { throw 'self-test begin marker is missing' }
    $summaries = @($lines | Where-Object { $_ -match '^\[FLINT\] SELFTEST END pass=([0-9]+) fail=([0-9]+)$' })
    if ($summaries.Count -ne 1) { throw 'exactly one complete self-test summary is required' }
    [void]($summaries[0] -match 'pass=([0-9]+) fail=([0-9]+)$')
    $reportedPass = [int]$Matches[1]
    $reportedFail = [int]$Matches[2]
    $passed = @($lines | Where-Object { $_ -match '^\[FLINT\] TEST .+ PASS$' }).Count
    $failed = @($lines | Where-Object { $_ -match '^\[FLINT\] TEST .+ FAIL(?: .*)?$' }).Count
    $skipped = @($lines | Where-Object { $_ -match '^\[FLINT\] TEST .+ SKIP .+$' }).Count
    $testLines = @($lines | Where-Object { $_ -match '^\[FLINT\] TEST ' })
    if ($testLines.Count -ne ($passed + $failed + $skipped)) { throw 'a test line has no valid PASS, FAIL, or SKIP disposition' }
    if ($passed -ne $reportedPass -or $failed -ne $reportedFail) { throw 'test lines do not agree with summary counts' }
    if ($reportedFail -ne 0 -or $reportedPass -eq 0) { throw "self-test did not pass: pass=$reportedPass fail=$reportedFail" }
    [pscustomobject]@{ state='passed'; passed=$reportedPass; failed=$reportedFail; skipped=$skipped } | ConvertTo-Json -Compress
    exit 0
}

function ConvertTo-Device {
    param([object]$Item, [string]$ParentInstanceId, [string]$LocationPath)
    $port = if ($Item.port) { [string]$Item.port } elseif ($Item.Name -match '\((COM[0-9]+)\)') { $Matches[1] } else { $null }
    $id = if ($Item.instance_id) { [string]$Item.instance_id } else { [string]$Item.PNPDeviceID }
    $deviceSerial = if ($Item.serial) { [string]$Item.serial } else {
        $identityId = if ($ParentInstanceId) { $ParentInstanceId } else { $id }
        $parts = $identityId -split '\\'
        if ($parts.Count -ge 3) { $parts[-1] } else { '' }
    }
    $deviceLocation = if ($Item.location_path) { [string]$Item.location_path } else { $LocationPath }
    [pscustomobject]@{ port = $port; instance_id = $id; serial = $deviceSerial; location_path = $deviceLocation }
}

function Get-LiveDevices {
    Get-CimInstance Win32_PnPEntity | Where-Object {
        $_.PNPDeviceID -match "^USB\\VID_$VendorId&PID_$ProductId" -and $_.Name -match '\(COM[0-9]+\)'
    } | ForEach-Object {
        $parent = (Get-PnpDeviceProperty -InstanceId $_.PNPDeviceID -KeyName 'DEVPKEY_Device_Parent').Data
        $location = @((Get-PnpDeviceProperty -InstanceId $parent -KeyName 'DEVPKEY_Device_LocationPaths').Data)[0]
        ConvertTo-Device $_ -ParentInstanceId $parent -LocationPath $location
    }
}

function Get-BootselDevices {
    # Select only the composite parent. Windows also enumerates mass-storage
    # and RP2 Boot children for the same board; counting those as boards made
    # both ambiguity detection and identity selection impossible.
    Get-CimInstance Win32_PnPEntity | Where-Object {
        $_.PNPDeviceID -match '^USB\\VID_2E8A&PID_0003\\([^\\]+)$'
    } | ForEach-Object {
        $serial = ([string]$_.PNPDeviceID -split '\\')[-1]
        $location = @((Get-PnpDeviceProperty -InstanceId $_.PNPDeviceID -KeyName 'DEVPKEY_Device_LocationPaths').Data)[0]
        [pscustomobject]@{ instance_id = [string]$_.PNPDeviceID; serial = $serial; location_path = $location }
    }
}

function Get-BootselSnapshot {
    if (-not $Fixture) { return @(Get-BootselDevices) }
    $snapshots = @($fixtureData.bootselSnapshots)
    if (-not $snapshots.Count) { return @() }
    $index = [Math]::Min($script:bootselSnapshotIndex, $snapshots.Count - 1)
    $script:bootselSnapshotIndex++
    return @($snapshots[$index])
}

function Select-BootselDevice {
    param([object[]]$Devices, [switch]$AllowMissing)
    $expectedLocation = if ($BootselLocationPath) { $BootselLocationPath } else { $script:expectedBootselLocationPath }
    $matches = @($Devices | Where-Object {
        (-not $BootselSerial -or $_.serial -eq $BootselSerial -or $_.instance_id -match "\\$([regex]::Escape($BootselSerial))$") -and
        (-not $expectedLocation -or $_.location_path -eq $expectedLocation)
    } | Sort-Object instance_id)
    if ($matches.Count -eq 0) {
        if ($AllowMissing) { return $null }
        throw "no BOOTSEL device matched VID=2E8A PID=0003$(if ($BootselSerial) { " serial=$BootselSerial" })$(if ($expectedLocation) { " location=$expectedLocation" })"
    }
    if ($matches.Count -gt 1) {
        throw 'multiple BOOTSEL devices matched; select one with -BootselSerial'
    }
    return $matches[0]
}

function Invoke-BoundedCopy {
    param([string]$Source, [string]$Destination)
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = (Get-Process -Id $PID).Path
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    [void]$start.ArgumentList.Add('-NoProfile')
    [void]$start.ArgumentList.Add('-Command')
    [void]$start.ArgumentList.Add('[IO.File]::Copy($args[0], $args[1], $true)')
    [void]$start.ArgumentList.Add($Source)
    [void]$start.ArgumentList.Add($Destination)
    $process = [System.Diagnostics.Process]::Start($start)
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        $process.Kill($true)
        throw "UF2 copy exceeded the $TimeoutSeconds second bound"
    }
    if ($process.ExitCode -ne 0) { throw "UF2 copy failed with exit $($process.ExitCode)" }
}

function Invoke-BaudReset {
    param([string]$Port)
    $serialPort = [System.IO.Ports.SerialPort]::new($Port, 1200)
    try {
        $serialPort.Open()
    } catch {
        # The SDK resets while Windows is completing Open(). The resulting
        # device-removal exception is neither success nor failure; enumeration
        # below is the authority.
    } finally {
        $serialPort.Dispose()
    }
}

$fixtureData = if ($Fixture) { Get-Content -Raw -LiteralPath $Fixture | ConvertFrom-Json } else { $null }
$script:snapshotIndex = 0
$script:bootselSnapshotIndex = 0
$script:expectedBootselLocationPath = $null
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

function Invoke-PicotoolReset {
    # Pico SDK's USB reset interface is what makes `picotool reboot -f`
    # supported. It is firmware-dependent; failure is evidence that the
    # running image did not expose a compatible reset interface.
    $arguments = @(
        'reboot', '-f', '-a',
        '--vid', "0x$($VendorId.ToLowerInvariant())",
        '--pid', "0x$($ProductId.ToLowerInvariant())"
    )
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $PicotoolPath
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $arguments) { [void]$start.ArgumentList.Add($argument) }
    try { $process = [System.Diagnostics.Process]::Start($start) }
    catch { throw "could not start picotool at '$PicotoolPath': $($_.Exception.Message)" }
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        $process.Kill($true)
        throw "picotool reset exceeded the $TimeoutSeconds second bound"
    }
    $stdout = $process.StandardOutput.ReadToEnd().Trim()
    $stderr = $process.StandardError.ReadToEnd().Trim()
    if ($process.ExitCode -ne 0) {
        throw "picotool reset failed with exit $($process.ExitCode): $stderr $stdout"
    }
}

if ($Action -eq 'await-test-bootsel') {
    if (Select-BootselDevice (Get-BootselSnapshot) -AllowMissing) { throw 'BOOTSEL was already present before the test observation began' }
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        $bootsel = Select-BootselDevice (Get-BootselSnapshot) -AllowMissing
        if ($bootsel) {
            [pscustomobject]@{ state='tests-passed'; vid='2E8A'; pid='0003'; serial=$bootsel.serial } | ConvertTo-Json -Compress
            exit 0
        }
        if (-not $Fixture) { Start-Sleep -Milliseconds $PollMilliseconds }
    }
    throw "test firmware did not enter RP2040 BOOTSEL within $TimeoutSeconds seconds"
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


if ($Action -eq 'reset-reconnect' -and -not $Fixture) {
    Invoke-PicotoolReset
}

if ($Action -eq 'bootsel-flash-reconnect' -and $Fixture) {
    $missing = Select-Device (Get-Snapshot) -AllowMissing
    if ($missing) { throw 'fixture did not model the CDC disconnect' }
    if (-not $fixtureData.bootsel -or -not $fixtureData.volume) { throw 'fixture did not model BOOTSEL and RPI-RP2 enumeration' }
    $returned = Select-Device (Get-Snapshot) -AllowMissing
    if (-not $returned) { throw 'fixture did not model application reconnect' }
    if ($returned.serial -ne $initial.serial) { throw 'fixture application identity changed' }
    Write-Device $returned 'reflashed-reconnected'
    exit 0
}

if ($Action -eq 'bootsel-flash-reconnect' -and -not $Fixture) {
    if (-not $Uf2Path) { throw '-Uf2Path is required for bootsel-flash-reconnect' }
    $resolvedUf2 = (Resolve-Path -LiteralPath $Uf2Path).Path
    # USB serial strings can change across application and ROM BOOTSEL modes.
    # The physical USB topology path does not, so bind the reset/flash to the
    # connector occupied by the selected application device.
    $script:expectedBootselLocationPath = $initial.location_path
    Invoke-BaudReset $initial.port
    $bootDeadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $sawDisconnect = $false
    $bootDevice = $null
    while ([DateTime]::UtcNow -lt $bootDeadline) {
        if (-not (Select-Device (Get-Snapshot) -AllowMissing)) { $sawDisconnect = $true }
        $bootDevice = Select-BootselDevice (Get-BootselDevices) -AllowMissing
        if ($sawDisconnect -and $bootDevice) { break }
        Start-Sleep -Milliseconds $PollMilliseconds
    }
    if (-not $sawDisconnect) { throw 'the original CDC device never disappeared after the 1200-baud reset' }
    if (-not $bootDevice) { throw 'BOOTSEL USB VID=2E8A PID=0003 did not appear within the bound' }
    $volumes = @(Get-Volume | Where-Object { $_.FileSystemLabel -eq 'RPI-RP2' -and $_.DriveLetter })
    if (-not $volumes.Count) { throw 'BOOTSEL enumerated, but no RPI-RP2 volume with a drive letter appeared' }
    if ($volumes.Count -ne 1) { throw 'multiple RPI-RP2 volumes are mounted; refusing an ambiguous flash target' }
    $volume = $volumes[0]
    Invoke-BoundedCopy $resolvedUf2 "$($volume.DriveLetter):\$(Split-Path -Leaf $resolvedUf2)"
    $returnDeadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $returnDeadline) {
        $current = Select-Device (Get-Snapshot) -AllowMissing
        if ($current) {
            if ($current.serial -ne $initial.serial) {
                throw "application returned with different identity: expected '$($initial.serial)', got '$($current.serial)'"
            }
            Write-Device $current 'reflashed-reconnected'
            exit 0
        }
        Start-Sleep -Milliseconds $PollMilliseconds
    }
    throw "the application CDC device did not return within $TimeoutSeconds seconds after UF2 copy"
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
