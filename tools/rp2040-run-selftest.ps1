# SPDX-License-Identifier: Apache-2.0
# Run the ARM acceptance image through a selected Debug Probe and judge its
# return to ROM BOOTSEL. The firmware enters BOOTSEL only after every test passes.

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$ElfPath,
    [Parameter(Mandatory)]
    [string]$ProbeSerial,
    [Parameter(Mandatory)]
    [string]$BootselSerial,
    [ValidateRange(5, 300)]
    [int]$TimeoutSeconds = 30,
    [string]$ProbeRsPath = 'probe-rs'
)

$ErrorActionPreference = 'Stop'
$elf = (Resolve-Path -LiteralPath $ElfPath).Path
$work = Join-Path (Split-Path -Parent $PSScriptRoot) 'target/tmp'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$stdout = Join-Path $work 'rp2040-selftest.stdout.log'
$stderr = Join-Path $work 'rp2040-selftest.stderr.log'
Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath $stdout, $stderr

function Test-ExpectedBootsel {
    @(Get-CimInstance Win32_PnPEntity | Where-Object {
        $_.PNPDeviceID -eq "USB\VID_2E8A&PID_0003\$BootselSerial"
    }).Count -eq 1
}

$arguments = @(
    'run', '--probe', "2e8a:000c:$ProbeSerial", '--chip', 'RP2040',
    '--protocol', 'swd', '--non-interactive', '--speed', '1000', $elf
)
$process = Start-Process -FilePath $ProbeRsPath -ArgumentList $arguments -NoNewWindow -PassThru `
    -RedirectStandardOutput $stdout -RedirectStandardError $stderr
$deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
$sawApplication = $false
$passed = $false
try {
    while ([DateTime]::UtcNow -lt $deadline) {
        $inBootsel = Test-ExpectedBootsel
        if (-not $inBootsel) { $sawApplication = $true }
        if ($sawApplication -and $inBootsel) { $passed = $true; break }
        if ($process.HasExited -and -not $inBootsel) { break }
        Start-Sleep -Milliseconds 50
    }
} finally {
    if (-not $process.HasExited) { $process.Kill($true) }
    $process.WaitForExit()
}

if (-not $passed) {
    $detail = ((Get-Content -ErrorAction SilentlyContinue -LiteralPath $stderr) -join "`n").Trim()
    if (-not $sawApplication) { throw "the selected target never left BOOTSEL: $detail" }
    throw "the self-test target did not return to BOOTSEL within $TimeoutSeconds seconds: $detail"
}

[pscustomobject]@{
    state = 'passed'
    evidence = 'target-left-and-returned-to-expected-bootsel'
    probe_serial = $ProbeSerial
    target_bootsel_serial = $BootselSerial
    measured = @(
        'sleep-timeout', 'queue-timeout',
        'nested-critical-sections', 'stack-guard', 'heap-allocation',
        'dual-core-affinity', 'cross-core-wakeup-soak', 'cross-core-spinlock-contention',
        'duplicate-execution-detection'
    )
    compile_only = @()
} | ConvertTo-Json -Compress
