# SPDX-License-Identifier: Apache-2.0
# Fixture-driven tests for tools/rp2040-target.ps1; no board is touched.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$harness = Join-Path $PSScriptRoot 'rp2040-target.ps1'
$work = Join-Path $root 'target/tmp/rp2040-harness-tests'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$failures = 0

function Invoke-Case {
    param([string]$Name, [object]$Data, [string[]]$Arguments, [int]$Want, [string]$Pattern)
    $fixture = Join-Path $work "$Name.json"
    ConvertTo-Json -InputObject $Data -Depth 8 | Set-Content -LiteralPath $fixture
    $output = & pwsh -NoProfile -File $harness @Arguments -Fixture $fixture 2>&1 | Out-String
    $actual = $LASTEXITCODE
    if ($actual -eq $Want -and $output -match $Pattern) { Write-Host "  ok    $Name"; return }
    Write-Host "  FAIL  $Name (exit $actual, wanted $Want): $output"
    $script:failures++
}

$device = @{ port='COM8'; instance_id='USB\VID_2E8A&PID_000A\E6614104032A4C2E'; serial='E6614104032A4C2E' }
Invoke-Case 'one-device' @($device) @('-Action','discover') 0 '"port":"COM8"'
Invoke-Case 'none' @() @('-Action','discover') 1 'no USB serial device matched'
$twoDevices = @($device, @{ port='COM9'; instance_id='USB\VID_2E8A&PID_000A\OTHER'; serial='OTHER' })
Invoke-Case -Name 'ambiguous' -Data $twoDevices -Arguments @('-Action','discover') -Want 1 -Pattern 'multiple USB serial devices matched'
Invoke-Case -Name 'serial-selects' -Data $twoDevices -Arguments @('-Action','discover','-Serial','OTHER') -Want 0 -Pattern '"port":"COM9"'
Invoke-Case 'reconnect' @{ snapshots=@(@($device), @(), @($device)) } @('-Action','observe-reconnect','-TimeoutSeconds','1') 0 '"state":"reconnected"'
Invoke-Case 'never-disconnected' @{ snapshots=@(@($device)) } @('-Action','observe-reconnect','-TimeoutSeconds','1') 1 'reset is not proven'
Invoke-Case 'wrong-device-returned' @{ snapshots=@(@($device), @(), @(@{ port='COM9'; instance_id='USB\VID_2E8A&PID_000A\OTHER'; serial='OTHER' })) } @('-Action','observe-reconnect','-TimeoutSeconds','1') 1 'different matching device appeared'

Remove-Item -Recurse -Force -LiteralPath $work
if ($failures) { throw "$failures RP2040 harness self-test(s) failed" }
Write-Host 'All RP2040 harness self-tests passed.'
