# SPDX-License-Identifier: Apache-2.0
# Run the ARM acceptance image through a selected Debug Probe and judge its
# return to ROM BOOTSEL. The firmware enters BOOTSEL only after every test passes.

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$ElfPath,
    [string]$ProbeSerial,
    [string]$SerialPort,
    [string]$Uf2Path,
    [Parameter(Mandatory)]
    [string]$BootselSerial,
    [ValidateSet('acceptance', 'watchdog-reset', 'diagnostics', 'dma', 'io', 'mutex')]
    [string]$Suite = 'acceptance',
    [ValidateRange(5, 300)]
    [int]$TimeoutSeconds = 30,
    [ValidateRange(1, 100)]
    [int]$Cycles = 1,
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

function Get-ExpectedBootselDrive {
    $disks = @(Get-Disk | Where-Object {
        $_.FriendlyName -like '*RP2*' -and $_.SerialNumber -eq $BootselSerial
    })
    if ($disks.Count -ne 1) {
        throw "expected one RP2 disk with serial $BootselSerial, found $($disks.Count)"
    }
    $partitions = @(Get-Partition -DiskNumber $disks[0].Number | Where-Object DriveLetter)
    if ($partitions.Count -ne 1) {
        throw "expected one mounted partition for RP2 serial $BootselSerial, found $($partitions.Count)"
    }
    "$($partitions[0].DriveLetter):"
}

function Write-Uf2([string]$Source, [string]$Destination) {
    $bytes = [IO.File]::ReadAllBytes($Source)
    $stream = [IO.FileStream]::new(
        $Destination,
        [IO.FileMode]::Create,
        [IO.FileAccess]::Write,
        [IO.FileShare]::None,
        4096,
        [IO.FileOptions]::WriteThrough
    )
    try {
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush($true)
    } finally {
        $stream.Dispose()
    }
}

function Enter-BootselViaWatchdog {
    $probe = "2e8a:000c:$ProbeSerial"
    $writes = @(
        @('0x40058020', '0'),
        @('0x4005801c', '0x6ab73121'),
        @('0x40010008', '0x0001fffc'),
        @('0x4005802c', '0x0000020c'),
        @('0x40058004', '0x00030d40'),
        @('0x4005a000', '0x40000000')
    )
    foreach ($write in $writes) {
        & $ProbeRsPath write --chip RP2040 --probe $probe b32 $write[0] $write[1] | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "could not arm BOOTSEL recovery through SWD" }
    }
}

if ($Suite -eq 'mutex') {
    if (-not $ProbeSerial) { throw '-ProbeSerial is required for the mutex suite' }
    $probe = "2e8a:000c:$ProbeSerial"
    $sysroot = (& rustc --print sysroot).Trim()
    $llvmNm = Join-Path $sysroot 'lib/rustlib/x86_64-pc-windows-msvc/bin/llvm-nm.exe'
    $statusLine = @(& $llvmNm -n $elf | Where-Object { $_ -match ' FLINT_RP2040_TEST_STATUS$' })
    if ($statusLine.Count -ne 1) { throw 'could not locate FLINT_RP2040_TEST_STATUS in the ELF' }
    $progressLine = @(& $llvmNm -n $elf | Where-Object { $_ -match ' MUTEX_SOAK_PROGRESS$' })
    if ($progressLine.Count -ne 1) { throw 'could not locate MUTEX_SOAK_PROGRESS in the ELF' }
    $statusAddress = '0x' + (($statusLine[0] -split '\s+')[0])
    $progressAddress = '0x' + (($progressLine[0] -split '\s+')[0])
    & $ProbeRsPath write --chip RP2040 --probe $probe b32 $statusAddress 0 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'could not clear the mutex test status through SWD' }
    & $ProbeRsPath write --chip RP2040 --probe $probe b32 $progressAddress 0 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'could not clear the mutex progress through SWD' }
    $passed = $false
    & $ProbeRsPath download --chip RP2040 --probe $probe --protocol swd `
        --non-interactive --speed 100 --preverify --verify --reset $elf | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'could not download the mutex test image through SWD' }

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        $status = (& $ProbeRsPath read --chip RP2040 --probe $probe `
            b32 $statusAddress 1 2>&1) -join "`n"
        if ($LASTEXITCODE -eq 0 -and $status -match ':\s+0000600d') {
            $passed = $true
            break
        }
        Start-Sleep -Milliseconds 100
    }
    if (-not $passed) { throw 'the mutex image did not publish its passing SWD status' }
    $progress = (& $ProbeRsPath read --chip RP2040 --probe $probe `
        b32 $progressAddress 1 2>&1) -join "`n"
    if ($LASTEXITCODE -ne 0 -or $progress -notmatch ':\s+000007d0') {
        throw "mutex status passed without exactly 2,000 completed cycles: $progress"
    }
    [pscustomobject]@{
        state = 'passed'
        evidence = 'mutex-priority-inheritance-2-cores-2000-cycles'
        probe_serial = $ProbeSerial
        transport = 'debugprobe-swd-download+swd-status+retained-progress'
    } | ConvertTo-Json -Compress
    exit 0
}

if ($Suite -eq 'io') {
    if (-not $ProbeSerial) { throw '-ProbeSerial is required for the I/O suite' }
    if (-not $SerialPort) { throw '-SerialPort is required for the I/O suite' }
    $probe = "2e8a:000c:$ProbeSerial"
    $sysroot = (& rustc --print sysroot).Trim()
    $llvmNm = Join-Path $sysroot 'lib/rustlib/x86_64-pc-windows-msvc/bin/llvm-nm.exe'
    $statusLine = @(& $llvmNm -n $elf | Where-Object { $_ -match ' FLINT_RP2040_TEST_STATUS$' })
    if ($statusLine.Count -ne 1) { throw 'could not locate FLINT_RP2040_TEST_STATUS in the ELF' }
    $statusAddress = '0x' + (($statusLine[0] -split '\s+')[0])
    & $ProbeRsPath write --chip RP2040 --probe $probe b32 $statusAddress 0 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'could not clear the I/O test status through SWD' }
    $port = [IO.Ports.SerialPort]::new($SerialPort, 115200, 'None', 8, 'One')
    $capture = ''
    $passed = $false
    try {
        $port.Open()
        $port.DtrEnable = $true
        Start-Sleep -Milliseconds 200
        & $ProbeRsPath download --chip RP2040 --probe $probe --protocol swd `
            --non-interactive --speed 100 --preverify --verify --reset $elf | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'could not download the I/O test image through SWD' }

        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        while ([DateTime]::UtcNow -lt $deadline) {
            $capture += $port.ReadExisting()
            if (Test-ExpectedBootsel) {
                $passed = $true
                break
            }
            $status = (& $ProbeRsPath read --chip RP2040 --probe $probe `
                b32 $statusAddress 1 2>&1) -join "`n"
            if ($LASTEXITCODE -eq 0 -and $status -match ':\s+0000600d') {
                $passed = $true
                break
            }
            Start-Sleep -Milliseconds 100
        }
        Start-Sleep -Milliseconds 200
        $capture += $port.ReadExisting()
    } finally {
        if ($port.IsOpen) { $port.Dispose() }
    }
    if (-not $passed) {
        throw 'the I/O image did not publish a passing status; verify the GP2-to-GP3 loopback jumper'
    }
    foreach ($marker in @(
        'ARM UART LOOPBACK payloads=1000 bytes=16000',
        'ARM GPIO LOOPBACK edges=10000'
    )) {
        if (-not $capture.Contains($marker)) { throw "UART output is missing '$marker'" }
    }
    [pscustomobject]@{
        state = 'passed'
        evidence = 'uart-1000-payloads+gpio-10000-exact-edges'
        probe_serial = $ProbeSerial
        target_bootsel_serial = $BootselSerial
        transport = 'debugprobe-swd-download+swd-status+uart'
    } | ConvertTo-Json -Compress
    exit 0
}

if ($Suite -eq 'dma') {
    if (-not $ProbeSerial) { throw '-ProbeSerial is required for the DMA suite' }
    if (-not $SerialPort) { throw '-SerialPort is required for the DMA suite' }

    $sysroot = (& rustc --print sysroot).Trim()
    $llvmNm = Join-Path $sysroot 'lib/rustlib/x86_64-pc-windows-msvc/bin/llvm-nm.exe'
    $statusLine = @(& $llvmNm -n $elf | Where-Object { $_ -match ' FLINT_RP2040_TEST_STATUS$' })
    if ($statusLine.Count -ne 1) { throw 'could not locate FLINT_RP2040_TEST_STATUS in the ELF' }
    $statusAddress = '0x' + (($statusLine[0] -split '\s+')[0])
    $probe = "2e8a:000c:$ProbeSerial"
    & $ProbeRsPath write --chip RP2040 --probe $probe b32 $statusAddress 0 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'could not clear the DMA test status through SWD' }

    $port = [IO.Ports.SerialPort]::new($SerialPort, 115200, 'None', 8, 'One')
    $capture = ''
    $passed = $false
    try {
        $port.Open()
        $port.DtrEnable = $true
        Start-Sleep -Milliseconds 200
        & $ProbeRsPath download --chip RP2040 --probe $probe --protocol swd `
            --non-interactive --speed 100 --preverify --verify --reset $elf | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'could not download the DMA test image through SWD' }

        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        while ([DateTime]::UtcNow -lt $deadline) {
            $capture += $port.ReadExisting()
            $status = (& $ProbeRsPath read --chip RP2040 --probe $probe `
                b32 $statusAddress 1 2>&1) -join "`n"
            if ($LASTEXITCODE -eq 0 -and $status -match ':\s+0000600d') {
                $passed = $true
                break
            }
            Start-Sleep -Milliseconds 100
        }
        Start-Sleep -Milliseconds 200
        $capture += $port.ReadExisting()
    } finally {
        if ($port.IsOpen) { $port.Dispose() }
    }
    if (-not $passed) { throw 'the DMA image did not publish its passing SWD status' }
    if (-not $capture.Contains('ARM DMA PASS rounds=100 bytes=512 timeout=ok')) {
        throw 'UART output is missing the DMA pass marker'
    }
    [pscustomobject]@{
        state = 'passed'
        evidence = 'dma-timeout-recovery-and-100x512-byte-uart-loopback'
        probe_serial = $ProbeSerial
        transport = 'debugprobe-swd-download+swd-status+uart'
    } | ConvertTo-Json -Compress
    exit 0
}

if ($Uf2Path -and $Suite -eq 'diagnostics') {
    if (-not $ProbeSerial) { throw '-ProbeSerial is required for the diagnostics suite' }
    if (-not $SerialPort) { throw '-SerialPort is required for the diagnostics suite' }
    if (-not (Test-ExpectedBootsel)) { throw 'the selected target is not in BOOTSEL' }
    $uf2 = (Resolve-Path -LiteralPath $Uf2Path).Path
    $targetDrive = Get-ExpectedBootselDrive

    $sysroot = (& rustc --print sysroot).Trim()
    $llvmNm = Join-Path $sysroot 'lib/rustlib/x86_64-pc-windows-msvc/bin/llvm-nm.exe'
    $statusLine = @(& $llvmNm -n $elf | Where-Object { $_ -match ' FLINT_RP2040_TEST_STATUS$' })
    if ($statusLine.Count -ne 1) { throw 'could not locate FLINT_RP2040_TEST_STATUS in the ELF' }
    $statusAddress = '0x' + (($statusLine[0] -split '\s+')[0])

    $port = [IO.Ports.SerialPort]::new($SerialPort, 115200, 'None', 8, 'One')
    $capture = ''
    $passed = $false
    try {
        $port.Open()
        # Debugprobe/TinyUSB considers the CDC link connected only with DTR set.
        $port.DtrEnable = $true
        Start-Sleep -Milliseconds 300
        Write-Uf2 $uf2 "$targetDrive\flint-diagnostics.uf2"

        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        while ([DateTime]::UtcNow -lt $deadline) {
            $capture += $port.ReadExisting()
            $status = (& $ProbeRsPath read --chip RP2040 --probe "2e8a:000c:$ProbeSerial" `
                b32 $statusAddress 1 2>&1) -join "`n"
            if ($LASTEXITCODE -eq 0 -and $status -match ':\s+0000600d') {
                $passed = $true
                break
            }
            Start-Sleep -Milliseconds 200
        }
        Start-Sleep -Milliseconds 300
        $capture += $port.ReadExisting()
    } finally {
        if ($port.IsOpen) { $port.Dispose() }
        if (-not (Test-ExpectedBootsel)) {
            Enter-BootselViaWatchdog
            $returnDeadline = [DateTime]::UtcNow.AddSeconds(10)
            while (-not (Test-ExpectedBootsel) -and [DateTime]::UtcNow -lt $returnDeadline) {
                Start-Sleep -Milliseconds 100
            }
        }
    }
    if (-not $passed) { throw 'the diagnostics image did not publish its passing SWD status' }
    foreach ($marker in @(
        'FlintOS booting...',
        'ARM DIAGNOSTICS counter=20000 gauge=143',
        'FLINT PANIC',
        'PREVIOUS BOOT PANICKED',
        'PC:',
        'State:',
        'ARM DIAGNOSTICS RECOVERED'
    )) {
        if (-not $capture.Contains($marker)) { throw "UART output is missing '$marker'" }
    }
    [pscustomobject]@{
        state = 'passed'
        evidence = 'uart-log-metrics-panic-snapshot-and-swd-status'
        target_bootsel_serial = $BootselSerial
        transport = 'bootsel-uf2+debugprobe-uart+swd-status'
    } | ConvertTo-Json -Compress
    exit 0
}

if ($Uf2Path) {
    $uf2 = (Resolve-Path -LiteralPath $Uf2Path).Path
    for ($cycle = 1; $cycle -le $Cycles; $cycle++) {
        if (-not (Test-ExpectedBootsel)) { throw "the selected target is not in BOOTSEL before cycle $cycle" }
        $targetDrive = Get-ExpectedBootselDrive
        Write-Uf2 $uf2 "$targetDrive\flint-$cycle.uf2"
        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        $sawApplication = $false
        $returned = $false
        while ([DateTime]::UtcNow -lt $deadline) {
            $inBootsel = Test-ExpectedBootsel
            if (-not $inBootsel) { $sawApplication = $true }
            if ($sawApplication -and $inBootsel) { $returned = $true; break }
            Start-Sleep -Milliseconds 50
        }
        if (-not $returned) {
            throw "the UF2 target did not leave and return to BOOTSEL in cycle $cycle within $TimeoutSeconds seconds (left=$sawApplication)"
        }
    }
    [pscustomobject]@{
        state = 'passed'
        evidence = 'uf2-target-left-and-returned-to-expected-bootsel'
        cycles = $Cycles
        target_bootsel_serial = $BootselSerial
        transport = 'bootsel-uf2'
    } | ConvertTo-Json -Compress
    exit 0
}

if (-not $ProbeSerial) { throw '-ProbeSerial is required when -Uf2Path is omitted' }

# RP2040 cannot answer SWD while RUN is held low. This deliberately lets the
# connect-under-reset attempt fail; probe-rs releases RUN while closing, and
# the real run operation below attaches during the measured recovery window.
& $ProbeRsPath reset --probe "2e8a:000c:$ProbeSerial" --chip RP2040 `
    --protocol swd --non-interactive --speed 100 --connect-under-reset 2>&1 | Out-Null

$arguments = @(
    'run', '--probe', "2e8a:000c:$ProbeSerial", '--chip', 'RP2040',
    '--protocol', 'swd', '--non-interactive', '--speed', '100', $elf
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
    measured = if ($Suite -eq 'watchdog-reset') {
        @('watchdog-timeout-reset', 'retained-watchdog-reset-cause')
    } else {
        @(
            'sleep-timeout', 'queue-timeout',
            'nested-critical-sections', 'stack-guard', 'heap-allocation',
            'mutex-priority-inheritance', 'task-isr-queue-race', 'mutex-under-tick-interruption',
            'dual-core-affinity', 'cross-core-wakeup-soak', 'cross-core-spinlock-contention',
            'duplicate-execution-detection'
        )
    }
    compile_only = @()
} | ConvertTo-Json -Compress
