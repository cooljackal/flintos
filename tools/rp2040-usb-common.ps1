# SPDX-License-Identifier: Apache-2.0
# Shared pure judges and bounded transport helpers for #172.
function Enter-UsbFixtureLock {
    param([string]$Location)
    if (-not $Location) { throw 'USB physical location is required for exclusive ownership' }
    $hash=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($Location.ToUpperInvariant())))
    $mutex=[Threading.Mutex]::new($false,"Local\FlintUsbFixture-$hash")
    try {
        try { $owned=$mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $owned=$true }
        if (-not $owned) { throw 'another USB harness owns this target; concurrent reset/flash is refused' }
        return $mutex
    } catch { $mutex.Dispose(); throw }
}

function Assert-UsbHello {
    param([byte[]]$Reply, [string]$ImageId, [byte[]]$Nonce)
    if ($ImageId -notmatch '^[0-9a-fA-F]{8}$' -or $Nonce.Length -ne 8) { throw 'invalid expected USB identity/nonce' }
    $expected = [Text.Encoding]::ASCII.GetBytes("F172$ImageId") + $Nonce
    if ($Reply.Length -ne 20 -or [Convert]::ToHexString($Reply) -cne [Convert]::ToHexString([byte[]]$expected)) {
        throw 'USB reply has wrong image, stale nonce, corrupt data, or wrong length'
    }
}

function Select-UsbFixtureDevice {
    param([object[]]$Devices, [string]$Kind, [string]$Location, [string]$RomSerial)
    if (-not $Location) { throw 'USB physical location is required; the private-test PID is not unique' }
    $found = @($Devices | Where-Object {
        $_.kind -eq $Kind -and $_.location -eq $Location -and $_.status -eq 'OK' -and
        ($Kind -ne 'rom' -or $_.serial -eq $RomSerial)
    })
    if ($found.Count -gt 1) { throw 'ambiguous USB device identity' }
    if ($found.Count -eq 1) { return $found[0] }
    return $null
}

function Get-UsbFixtureSnapshot {
    $devices = @(Get-PnpDevice -PresentOnly)
    foreach ($parent in $devices | Where-Object { $_.InstanceId -match '^USB\\VID_(1209&PID_0001|2E8A&PID_0003)\\' }) {
        $location = @((Get-PnpDeviceProperty -InstanceId $parent.InstanceId -KeyName DEVPKEY_Device_LocationPaths -ErrorAction SilentlyContinue).Data)[0]
        $kind = if ($parent.InstanceId -match 'VID_1209') { 'app' } else { 'rom' }
        $port = $null
        if ($kind -eq 'app') {
            foreach ($child in $devices | Where-Object { $_.InstanceId -match '^USB\\VID_1209&PID_0001&MI_00\\' -and $_.FriendlyName -match '\(COM\d+\)' }) {
                $owner = (Get-PnpDeviceProperty -InstanceId $child.InstanceId -KeyName DEVPKEY_Device_Parent).Data
                if ($owner -eq $parent.InstanceId -and $child.FriendlyName -match '\((COM\d+)\)') { $port = $Matches[1] }
            }
        }
        [pscustomobject]@{kind=$kind; location=$location; status=[string]$parent.Status; instance=$parent.InstanceId; serial=($parent.InstanceId -split '\\')[-1]; port=$port}
    }
}

function Wait-UsbFixtureDevice {
    param([string]$Kind, [string]$Location, [string]$RomSerial, [int]$TimeoutSeconds=25)
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $device = Select-UsbFixtureDevice @(Get-UsbFixtureSnapshot) $Kind $Location $RomSerial
        if ($device -and ($Kind -ne 'app' -or $device.port)) { return $device }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "$Kind USB device did not appear at the selected physical location within $TimeoutSeconds seconds"
}

function Invoke-UsbBoundedProcess {
    param([string]$Program, [string[]]$Arguments, [int]$TimeoutSeconds=30)
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName=$Program; $start.UseShellExecute=$false; $start.CreateNoWindow=$true
    $start.RedirectStandardOutput=$true; $start.RedirectStandardError=$true
    foreach ($argument in $Arguments) { [void]$start.ArgumentList.Add($argument) }
    $process=[Diagnostics.Process]::Start($start)
    try {
        $stdout=$process.StandardOutput.ReadToEndAsync(); $stderr=$process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutSeconds*1000)) { $process.Kill($true); throw "$Program exceeded its $TimeoutSeconds second deadline" }
        $output=$stdout.GetAwaiter().GetResult()+$stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "$Program failed ($($process.ExitCode)): $output" }
        return $output.Trim()
    } finally { $process.Dispose() }
}

# A transport failure gets exactly one independent recovery attempt. Firmware
# identity/data verification stays outside the catch: bad results cannot be
# hidden by reflashing and trying again.
function Invoke-UsbRecoverableUpdate {
    [CmdletBinding()]
    param([scriptblock]$Update, [scriptblock]$Recover, [scriptblock]$Verify)
    $recovered=$false
    try { $device=& $Update }
    catch {
        Write-Warning "USB update transport failed; trying one bounded SWD recovery: $_"
        $device=& $Recover
        $recovered=$true
    }
    & $Verify $device
    return [pscustomobject]@{device=$device;recovered=$recovered}
}

function Get-UsbRomVolume {
    param([object]$Device)
    $matches = @(
        foreach ($disk in Get-CimInstance Win32_DiskDrive | Where-Object { $_.PNPDeviceID -like 'USBSTOR\*' }) {
            $parent=$disk.PNPDeviceID
            for ($depth=0; $depth -lt 6 -and $parent -and $parent -ne $Device.instance; $depth++) {
                $parent=(Get-PnpDeviceProperty -InstanceId $parent -KeyName DEVPKEY_Device_Parent -ErrorAction SilentlyContinue).Data
            }
            if ($parent -ne $Device.instance) { continue }
            foreach ($partition in Get-CimAssociatedInstance -InputObject $disk -Association Win32_DiskDriveToDiskPartition) {
                foreach ($drive in Get-CimAssociatedInstance -InputObject $partition -Association Win32_LogicalDiskToPartition) {
                    if ($drive.VolumeName -eq 'RPI-RP2') { $drive.DeviceID + '\' }
                }
            }
        }
    )
    if ($matches.Count -ne 1) { throw 'selected ROM USB device must own exactly one RPI-RP2 volume' }
    return $matches[0]
}

function Open-UsbSerial {
    param([string]$Port)
    $serial=[IO.Ports.SerialPort]::new($Port,115200)
    $serial.DtrEnable=$true; $serial.ReadTimeout=3000; $serial.WriteTimeout=3000
    try { $serial.Open(); $serial.DiscardInBuffer(); return $serial }
    catch { $serial.Dispose(); throw }
}

function Read-UsbExact {
    param([IO.Ports.SerialPort]$Serial,[int]$Length)
    $bytes=[byte[]]::new($Length); $count=0; $deadline=[DateTime]::UtcNow.AddSeconds(5)
    while ($count -lt $Length) {
        if ([DateTime]::UtcNow -ge $deadline) { throw 'USB response exceeded its total deadline' }
        $count += $Serial.Read($bytes,$count,$Length-$count)
    }
    return ,$bytes
}

function Send-UsbCommand {
    param([IO.Ports.SerialPort]$Serial,[byte]$Command,[byte[]]$Argument)
    if ($Argument.Length -gt 8) { throw 'USB command argument is too long' }
    $packet=[byte[]]::new(16); [Text.Encoding]::ASCII.GetBytes('F172').CopyTo($packet,0)
    $packet[4]=$Command; $Argument.CopyTo($packet,8); $Serial.Write($packet,0,16)
}

function Test-UsbHello {
    param([IO.Ports.SerialPort]$Serial,[string]$ImageId)
    $nonce=[byte[]]::new(8); [Security.Cryptography.RandomNumberGenerator]::Fill($nonce)
    Send-UsbCommand $Serial 0 $nonce
    Assert-UsbHello (Read-UsbExact $Serial 20) $ImageId $nonce
}

function Test-UsbEcho {
    param([IO.Ports.SerialPort]$Serial,[int]$Length)
    if ($Length -lt 1 -or $Length -gt 1048576) { throw 'invalid echo length' }
    Send-UsbCommand $Serial 1 ([BitConverter]::GetBytes([uint32]$Length))
    $offset=0
    while ($offset -lt $Length) {
        $count=[Math]::Min(256,$Length-$offset); $data=[byte[]]::new($count)
        for ($i=0;$i -lt $count;$i++) { $data[$i]=(($offset+$i)*37+11) -band 255 }
        $Serial.Write($data,0,$count); $reply=Read-UsbExact $Serial $count
        if ([Convert]::ToHexString($reply) -cne [Convert]::ToHexString($data)) { throw "USB echo mismatch at $offset/$Length" }
        $offset+=$count
    }
}
