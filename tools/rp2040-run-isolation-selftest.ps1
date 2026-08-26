# SPDX-License-Identifier: Apache-2.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ElfPath,
    [Parameter(Mandatory)][string]$ProbeSerial,
    [Parameter(Mandatory)][string]$SerialPort,
    [Parameter(Mandatory)][string]$LocationPath,
    [string]$BootselSerial='E0C912D24340',
    [switch]$UnexpectedFault,
    [ValidateSet(0,1)][int]$FaultCore=0
)
$ErrorActionPreference='Stop'
. "$PSScriptRoot/rp2040-usb-common.ps1"
. "$PSScriptRoot/rp2040-isolation-common.ps1"
$elf=(Resolve-Path -LiteralPath $ElfPath).Path
$uf2=$elf+'.uf2'
$probe="2e8a:000c:$ProbeSerial"
$sysroot=(Invoke-UsbBoundedProcess 'rustc' @('--print','sysroot') 10).Trim()
$nm=Join-Path $sysroot 'lib/rustlib/x86_64-pc-windows-msvc/bin/llvm-nm.exe'
$symbols=Invoke-UsbBoundedProcess $nm @('-n',$elf) 10
$addresses=@{}
$requiredSymbols=@('ISOLATION_RESULTS','ISOLATION_NONCE')
if ($UnexpectedFault) { $requiredSymbols+=@('FLINT_ISOLATION_FAULT','ISOLATION_UNDEFINED_PC','ISOLATION_FAULT_CORE') }
foreach ($symbol in $requiredSymbols) {
    $found=[regex]::Matches($symbols,"(?m)^([0-9a-fA-F]+)\s+\w\s+$symbol\r?`$")
    if ($found.Count -ne 1) { throw "could not find $symbol" }
    $addresses[$symbol]='0x'+$found[0].Groups[1].Value
}
function Read-IsolationWords {
    param([string]$Address=$addresses['ISOLATION_RESULTS'],[int]$Count=8)
    $raw=Invoke-UsbBoundedProcess 'probe-rs' @('read','--chip','RP2040','--probe',$probe,'b32',$Address,[string]$Count) 10
    $lines=[regex]::Matches($raw,'(?m)^\s*(?:0x)?[0-9a-fA-F]+:\s+([0-9a-fA-F ]+)\r?$')
    $words=@(foreach ($line in $lines) { foreach ($word in ($line.Groups[1].Value.Trim() -split '\s+')) {
        if ($word -notmatch '^[0-9a-fA-F]{8}$') { throw 'malformed SWD word' }
        [Convert]::ToUInt32($word,16)
    } })
    if ($words.Count -ne $Count) { throw "expected $Count isolation result words: $raw" }
    return ,([uint32[]]$words)
}
$fixtureLock=Enter-UsbFixtureLock $LocationPath
$uart=[IO.Ports.SerialPort]::new($SerialPort,115200)
$console=''; $words=$null
try {
    Invoke-UsbBoundedProcess 'pwsh' @('-NoProfile','-File',"$PSScriptRoot/rp2040-image.ps1",'-Action','convert','-Architecture','armv6m','-Soc','rp2040','-Board','raspberry-pi-pico','-Elf',$elf,'-Bin',($elf+'.bin'),'-Uf2',$uf2) 15 | Out-Null
    $uart.Open(); $uart.DtrEnable=$true; $uart.DiscardInBuffer()
    # Start the loader with both CPUs reset, not an attached user context.
    # Match the physical target before copying its freshly derived UF2.
    $rom=Select-UsbFixtureDevice @(Get-UsbFixtureSnapshot) 'rom' $LocationPath $BootselSerial
    if (-not $rom) {
        foreach ($write in @(@('0x40058020','0'),@('0x4005801c','0x6ab73121'),@('0x40010008','0x0001fffc'),@('0x4005802c','0x0000020c'),@('0x40058004','0x00030d40'),@('0x4005a000','0x40000000'))) {
            Invoke-UsbBoundedProcess 'probe-rs' @('write','--chip','RP2040','--probe',$probe,'b32',$write[0],$write[1]) 10 | Out-Null
        }
        try { $rom=Wait-UsbFixtureDevice 'rom' $LocationPath $BootselSerial 12 }
        catch {
            # One bounded transport alternative after reset; never repeat a
            # failed result verification. A nonce is still mandatory below.
            Write-Warning 'ROM USB absent after watchdog reset; using one SWD download'
            Invoke-UsbBoundedProcess 'probe-rs' @('download','--chip','RP2040','--probe',$probe,'--protocol','swd','--non-interactive','--speed','100','--preverify','--verify','--reset',$elf) 60 | Out-Host
        }
    }
    if ($rom) {
        $volume=Get-UsbRomVolume $rom
        Invoke-UsbBoundedProcess 'pwsh' @('-NoProfile','-File',"$PSScriptRoot/rp2040-usb-copy.ps1",'-Source',$uf2,'-Destination',$volume) 30 | Out-Null
    }
    Start-Sleep -Milliseconds 500
    $deadline=[DateTime]::UtcNow.AddSeconds(10)
    do {
        $words=Read-IsolationWords; $console+=$uart.ReadExisting()
        if ($words[0] -ne 0) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    $gate=if ($UnexpectedFault) { 0x139f0001 } else { 0x13900001 }
    if ($words[0] -ne $gate -or $words[1] -ne 0) { throw 'isolation did not reach fresh nonce gate' }
    $nonce=[uint32][Security.Cryptography.RandomNumberGenerator]::GetInt32(1,[int]::MaxValue)
    if ($UnexpectedFault) {
        Invoke-UsbBoundedProcess 'probe-rs' @('write','--chip','RP2040','--probe',$probe,'b32',$addresses['ISOLATION_FAULT_CORE'],[string]$FaultCore) 10 | Out-Null
    }
    Invoke-UsbBoundedProcess 'probe-rs' @('write','--chip','RP2040','--probe',$probe,'b32',$addresses['ISOLATION_NONCE'],('0x{0:x8}' -f $nonce)) 10 | Out-Null
    Start-Sleep -Seconds 6
    if ($UnexpectedFault) {
        $words=Read-IsolationWords
        $faultAddress='0x{0:x8}' -f ([Convert]::ToUInt32($addresses['FLINT_ISOLATION_FAULT'].Substring(2),16)+48*$FaultCore)
        $fault=Read-IsolationWords $faultAddress 12
        $after=Read-IsolationWords $addresses['ISOLATION_NONCE'] 1
        $console+=$uart.ReadExisting()
        $pc=[Convert]::ToUInt32($addresses['ISOLATION_UNDEFINED_PC'].Substring(2),16)
        Assert-Rp2040IsolationFault $fault $words[0] $after[0] $nonce $pc $console $FaultCore
        [pscustomobject]@{state='isolation-unexpected-fault-pass';nonce=$nonce;task=$fault[1];core=$FaultCore;pc=('0x{0:x8}' -f $pc);retained_panic=$true;manual_bootsel=$false} | ConvertTo-Json -Compress
        Write-Output $console
        return
    }
    $deadline=[DateTime]::UtcNow.AddSeconds(20)
    do {
        $words=Read-IsolationWords; $console+=$uart.ReadExisting()
        if ($words[0] -eq 0x1390600d -or ($words[0] -shr 16) -eq 0xbad1) { break }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    Assert-Rp2040IsolationResult $words $nonce $console
    [pscustomobject]@{state='isolation-pass';nonce=$nonce;faults=$words[2];rejected=$words[3];iterations=$words[4];cores=$words[5];activations_core0=$words[6];activations_core1=$words[7];manual_bootsel=$false} | ConvertTo-Json -Compress
    Write-Output $console
} catch {
    if ($words) { Write-Output ('Isolation words: '+(($words | ForEach-Object { '{0:x8}' -f $_ }) -join ' ')) }
    if ($uart.IsOpen) { $console+=$uart.ReadExisting() }
    Write-Output $console; throw
} finally { $uart.Dispose(); $fixtureLock.ReleaseMutex(); $fixtureLock.Dispose() }
