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
. "$PSScriptRoot/rp2040-pio-common.ps1"
$elf=(Resolve-Path -LiteralPath $ElfPath).Path
$probe="2e8a:000c:$ProbeSerial"
$sysroot=(Invoke-UsbBoundedProcess 'rustc' @('--print','sysroot') 10).Trim()
$nm=Join-Path $sysroot 'lib/rustlib/x86_64-pc-windows-msvc/bin/llvm-nm.exe'
$symbols=Invoke-UsbBoundedProcess $nm @('-n',$elf) 10
$addresses=@{}
foreach ($symbol in @('PIO_RESULTS','PIO_NONCE')) {
    $found=[regex]::Matches($symbols,"(?m)^([0-9a-fA-F]+)\s+\w\s+$symbol\r?`$")
    if ($found.Count -ne 1) { throw "could not find $symbol" }
    $addresses[$symbol]='0x'+$found[0].Groups[1].Value
}
function Read-PioWords {
    $raw=Invoke-UsbBoundedProcess 'probe-rs' @('read','--chip','RP2040','--probe',$probe,'b32',$addresses['PIO_RESULTS'],'10') 10
    $lines=[regex]::Matches($raw,'(?m)^\s*(?:0x)?[0-9a-fA-F]+:\s+([0-9a-fA-F ]+)\r?$')
    $words=@(foreach ($line in $lines) { foreach ($word in ($line.Groups[1].Value.Trim() -split '\s+')) {
        if ($word -notmatch '^[0-9a-fA-F]{8}$') { throw 'malformed SWD word' }
        [Convert]::ToUInt32($word,16)
    } })
    if ($words.Count -ne 10) { throw "expected 10 PIO result words: $raw" }
    return ,([uint32[]]$words)
}
$fixtureLock=Enter-UsbFixtureLock $LocationPath
$uart=[IO.Ports.SerialPort]::new($SerialPort,115200)
$console=''; $words=$null
try {
    $uart.Open(); $uart.DtrEnable=$true; $uart.DiscardInBuffer()
    Invoke-UsbBoundedProcess 'probe-rs' @('download','--chip','RP2040','--probe',$probe,'--protocol','swd','--non-interactive','--speed','100','--preverify','--verify','--reset',$elf) 60 | Out-Host
    $deadline=[DateTime]::UtcNow.AddSeconds(10)
    do {
        $words=Read-PioWords; $console+=$uart.ReadExisting()
        if ($words[0] -ne 0) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($words[0] -ne 0x17500001 -or $words[1] -ne 0) { throw 'PIO did not reach fresh nonce gate' }
    $nonce=[uint32][Security.Cryptography.RandomNumberGenerator]::GetInt32(1,[int]::MaxValue)
    Invoke-UsbBoundedProcess 'probe-rs' @('write','--chip','RP2040','--probe',$probe,'b32',$addresses['PIO_NONCE'],('0x{0:x8}' -f $nonce)) 10 | Out-Null
    # Do not repeatedly attach a debugger during the timeout/payload tests.
    Start-Sleep -Seconds 6
    $deadline=[DateTime]::UtcNow.AddSeconds(20)
    do {
        $words=Read-PioWords; $console+=$uart.ReadExisting()
        if ($words[0] -eq 0x1750600d -or ($words[0] -shr 16) -eq 0xbad1) { break }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    Assert-Rp2040PioResult $words $nonce $ExpectedHz $console
    [pscustomobject]@{state='pio-pass';nonce=$nonce;words=$words[2];blocks=$words[3];timeout_recoveries=$words[4];contention_rejections=$words[5];fifo_full=$words[6];fifo_empty=$words[7];reopens=$words[8];cpu_hz=$words[9];manual_bootsel=$false} | ConvertTo-Json -Compress
    Write-Output $console
} catch {
    if ($words) { Write-Output ('PIO words: '+(($words | ForEach-Object { '{0:x8}' -f $_ }) -join ' ')) }
    if ($uart.IsOpen) { $console+=$uart.ReadExisting() }
    Write-Output $console; throw
} finally { $uart.Dispose(); $fixtureLock.ReleaseMutex(); $fixtureLock.Dispose() }
