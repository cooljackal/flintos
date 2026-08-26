# SPDX-License-Identifier: Apache-2.0
# No SWD polling while the target changes XIP. Completion comes over UART.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ElfPath,
    [Parameter(Mandatory)][string]$ProbeSerial,
    [Parameter(Mandatory)][string]$SerialPort,
    [Parameter(Mandatory)][switch]$EraseReservedNvs,
    [ValidateRange(1, 20)][int]$Cycles = 1,
    [ValidateRange(5, 60)][int]$TimeoutSeconds = 30
)
$ErrorActionPreference = 'Stop'
if (-not $EraseReservedNvs) { throw 'This test destroys the board-reserved final 16 KiB NVS partition.' }
$elf = (Resolve-Path -LiteralPath $ElfPath).Path
$probe = "2e8a:000c:$ProbeSerial"
$llvm = Join-Path ((& rustc --print sysroot).Trim()) 'lib/rustlib/x86_64-pc-windows-msvc/bin'
$nm = @(& "$llvm/llvm-nm.exe" -n $elf)
$addresses = @{}
foreach ($symbol in @('FLASH_GO', 'FLASH_STAGE', 'FLASH_PEER_RUNS', 'FLASH_PROGRAM_US', 'FLASH_ERASE_US', 'FLASH_TIMEOUT_US', 'FLASH_WDT_BEFORE', 'FLASH_WDT_AFTER', 'FLASH_STALL_MAGIC', 'FLASH_STALL_US', 'FLINT_RP2040_TEST_STATUS')) {
    $lines = @($nm | Where-Object { $_ -match " $symbol`$" })
    if ($lines.Count -ne 1) { throw "missing flash-suite symbol $symbol" }
    $addresses[$symbol] = '0x' + ($lines[0] -split '\s+')[0]
}

function Read-Word([string]$Symbol) {
    $raw = (& probe-rs read --chip RP2040 --probe $probe b32 $addresses[$Symbol] 1 2>&1) -join "`n"
    if ($LASTEXITCODE -ne 0 -or $raw -notmatch ':\s+([0-9a-fA-F]{8})') { throw "cannot read ${Symbol}: $raw" }
    [Convert]::ToUInt32($Matches[1], 16)
}

$serial = [IO.Ports.SerialPort]::new($SerialPort, 115200)
$serial.DtrEnable = $true
$serial.ReadTimeout = 100
$serial.Open()
try {
    for ($cycle = 1; $cycle -le $Cycles; $cycle++) {
        $serial.DiscardInBuffer()
        & probe-rs download --chip RP2040 --probe $probe --protocol swd --non-interactive --speed 100 --preverify --verify --reset $elf
        if ($LASTEXITCODE -ne 0) { throw 'flash-suite download failed' }
        foreach ($phase in @(1, 2, 3, 2, 4, 2)) {
            if ($phase -ne 1) {
                & probe-rs reset --chip RP2040 --probe $probe
                if ($LASTEXITCODE -ne 0) { throw 'persistence reset failed' }
            }
            # Poll only before GO, when the firmware promises XIP remains on.
            $deadline = [DateTime]::UtcNow.AddSeconds(10)
            while ((Read-Word 'FLASH_STAGE') -ne 1) {
                if ([DateTime]::UtcNow -ge $deadline) { throw 'flash-suite did not reach its safe GO gate' }
                Start-Sleep -Milliseconds 100
            }
            $serial.DiscardInBuffer()
            & probe-rs write --chip RP2040 --probe $probe b32 $addresses['FLASH_GO'] $phase
            if ($LASTEXITCODE -ne 0) { throw 'could not release flash-suite GO gate' }
            $expected = switch ($phase) { 1 { 'FLASH WRITE PASS' } 2 { 'FLASH PERSIST PASS' } default { 'FLASH STALL' } }
            $received = ''
            $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
            while ($received -notmatch $expected -and $received -notmatch 'FLASH FAIL') {
                if ([DateTime]::UtcNow -ge $deadline) {
                    # The per-operation watchdog has a three-second maximum.
                    # No repeated reset/flashing retry or live-XIP polling.
                    throw "UART completion timeout in phase $phase; output: $received"
                }
                $received += $serial.ReadExisting()
                Start-Sleep -Milliseconds 50
            }
            if ($phase -ge 3 -and $received -match 'FLASH STALL') {
                $watch = [Diagnostics.Stopwatch]::StartNew()
                Start-Sleep -Milliseconds $(if ($phase -eq 3) { 3500 } else { 2500 })
                # Read only watchdog registers, never XIP. The early Reset
                # handler consumes our marker before entering ROM recovery;
                # ROM installs a checked reboot vector in scratch[5..7]. This
                # proves a reset even when the target USB cable is absent.
                $raw = (& probe-rs read --chip RP2040 --probe $probe b32 0x40058008 9 2>&1) -join "`n"
                if ($LASTEXITCODE -ne 0) { throw "cannot read watchdog recovery evidence: $raw" }
                $words = @($raw -split "`n" | Where-Object { $_ -match '^400580[0-9a-fA-F]{2}:' } | ForEach-Object {
                    ($_ -split ':')[1].Trim() -split '\s+' | ForEach-Object { [Convert]::ToUInt32($_, 16) }
                })
                if ($words.Count -ne 9 -or ($words[0] -band 1) -ne 1 -or $words[5] -ne 0 -or
                    $words[8] -ge 0x4000 -or ($words[8] -band 1) -ne 1 -or
                    $words[6] -ne ($words[8] -bxor 0x4ff83f2d) -or $words[7] -ne 0x20042000) {
                    throw "watchdog timeout/consumed marker/ROM recovery vector mismatch: $raw"
                }
                $stallMagic = Read-Word 'FLASH_STALL_MAGIC'
                $stallUs = Read-Word 'FLASH_STALL_US'
                if ($stallMagic -ne 0x171fdeadu -or
                    ($phase -eq 3 -and ($stallUs -lt 2500000 -or $stallUs -gt 3200000)) -or
                    ($phase -eq 4 -and ($stallUs -lt 500000 -or $stallUs -gt 1500000))) {
                    throw "watchdog stall duration did not preserve the expected deadline: magic=$stallMagic elapsed_us=$stallUs"
                }
                [pscustomobject]@{cycle=$cycle; phase=$phase; state='watchdog-reset-observed'; stall_us=$stallUs; detection_ms=$watch.ElapsedMilliseconds; usb_required=$false} | ConvertTo-Json -Compress
                # Automatic recovery, then the next phase verifies the keys again.
                & probe-rs download --chip RP2040 --probe $probe --protocol swd --non-interactive --speed 100 --preverify --verify --reset $elf
                if ($LASTEXITCODE -ne 0) { throw 'automatic reload after XIP-off stall failed' }
                continue
            }
            # A terminal UART marker means there are no further flash writes.
            $measurements = @{}
            foreach ($symbol in $addresses.Keys | Where-Object { $_ -ne 'FLASH_GO' }) {
                $measurements[$symbol] = Read-Word $symbol
            }
            if ($received -match 'FLASH FAIL' -or $measurements['FLINT_RP2040_TEST_STATUS'] -ne 0x600d) {
                throw "flash-suite failure: $($measurements | ConvertTo-Json -Compress)"
            }
            [pscustomobject]@{cycle=$cycle; phase=$phase; state='passed'; measurements=$measurements} | ConvertTo-Json -Compress
        }
    }
} finally {
    $serial.Dispose()
}
