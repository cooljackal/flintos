# SPDX-License-Identifier: Apache-2.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ElfPath,
    [Parameter(Mandatory)][string]$ProbeSerial,
    [Parameter(Mandatory)][string]$SerialPort,
    [Parameter(Mandatory)][string]$LocationPath,
    [Parameter(Mandatory)][ValidateSet(12000000,125000000)][uint32]$ExpectedHz
)
$ErrorActionPreference='Stop'
. "$PSScriptRoot/rp2040-usb-common.ps1"
. "$PSScriptRoot/rp2040-clock-common.ps1"
$elf=(Resolve-Path -LiteralPath $ElfPath).Path
$probe="2e8a:000c:$ProbeSerial"
$sysroot=(Invoke-UsbBoundedProcess 'rustc' @('--print','sysroot') 10).Trim()
$nm=Join-Path $sysroot 'lib/rustlib/x86_64-pc-windows-msvc/bin/llvm-nm.exe'
$symbols=Invoke-UsbBoundedProcess $nm @('-n',$elf) 10
$addresses=@{}
foreach ($symbol in @('CLOCK_RESULTS','CLOCK_NONCE')) {
    $found=[regex]::Matches($symbols,"(?m)^([0-9a-fA-F]+)\s+\w\s+$symbol\r?`$")
    if ($found.Count -ne 1) { throw "could not find $symbol in the clock test image" }
    $addresses[$symbol]='0x'+$found[0].Groups[1].Value
}
function Read-ClockWords {
    $raw=Invoke-UsbBoundedProcess 'probe-rs' @('read','--chip','RP2040','--probe',$probe,'b32',$addresses['CLOCK_RESULTS'],'11') 10
    $lines=[regex]::Matches($raw,'(?m)^\s*(?:0x)?[0-9a-fA-F]+:\s+([0-9a-fA-F ]+)\r?$')
    $words=@(foreach ($line in $lines) { foreach ($word in ($line.Groups[1].Value.Trim() -split '\s+')) {
        if ($word -notmatch '^[0-9a-fA-F]{8}$') { throw 'malformed SWD word' }
        [Convert]::ToUInt32($word,16)
    } })
    if ($words.Count -ne 11) { throw "expected 11 retained clock words, received $($words.Count): $raw" }
    return ,([uint32[]]$words)
}
$fixtureLock=Enter-UsbFixtureLock $LocationPath
$uart=[IO.Ports.SerialPort]::new($SerialPort,115200)
try {
    $uart.Open(); $uart.DtrEnable=$true; $uart.DiscardInBuffer()
    Invoke-UsbBoundedProcess 'probe-rs' @('download','--chip','RP2040','--probe',$probe,'--protocol','swd','--non-interactive','--speed','100','--preverify','--verify','--reset',$elf) 60 | Out-Host
    $console=$uart.ReadExisting()
    $readyDeadline=[DateTime]::UtcNow.AddSeconds(10)
    do {
        $words=Read-ClockWords
        $console+=$uart.ReadExisting()
        if ($words[0] -ne 0) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $readyDeadline)
    if ($words[0] -ne 0x17400001 -or $words[1] -ne 0) { throw 'clock test did not reach its fresh nonce gate' }
    $nonce=[uint32][Security.Cryptography.RandomNumberGenerator]::GetInt32(1,[int]::MaxValue)
    Invoke-UsbBoundedProcess 'probe-rs' @('write','--chip','RP2040','--probe',$probe,'b32',$addresses['CLOCK_NONCE'],('0x{0:x8}' -f $nonce)) 10 | Out-Null
    # Let the sampling/tick check finish without debugger attachment. RP2040
    # TIMER_DBGPAUSE defaults to pausing on either halted core, while the other
    # core can still run SysTick. Read the retained result after this quiet window.
    Start-Sleep -Seconds 3
    $deadline=[DateTime]::UtcNow.AddSeconds(20)
    do {
        $words=Read-ClockWords
        $console+=$uart.ReadExisting()
        if ($words[0] -eq 0x1740600d -or ($words[0] -shr 16) -eq 0xbad0) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    Assert-Rp2040ClockResult $words $nonce $ExpectedHz $console
    [pscustomobject]@{state='clock-pass';nonce=$nonce;configured_hz=$words[2];boot_hz=$words[3];min_hz=$words[4];max_hz=$words[5];core0_samples=$words[6];core1_samples=$words[7];busy_retries=$words[8];elapsed_us=$words[9];elapsed_ticks=$words[10];tolerance_hz=5000;manual_bootsel=$false} | ConvertTo-Json -Compress
    Write-Output $console
} catch {
    if ($words) { Write-Output ('clock words: '+(($words | ForEach-Object { '{0:x8}' -f $_ }) -join ' ')) }
    if ($uart.IsOpen) { $console+=$uart.ReadExisting() }
    Write-Output $console
    throw
} finally { $uart.Dispose(); $fixtureLock.ReleaseMutex(); $fixtureLock.Dispose() }
