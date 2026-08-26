# SPDX-License-Identifier: Apache-2.0
$ErrorActionPreference='Stop'
. "$PSScriptRoot/rp2040-usb-common.ps1"
$script:checks=0
function Expect-Failure([scriptblock]$Case) {
    $failed=$false
    try { & $Case | Out-Null } catch { $failed=$true }
    if (-not $failed) { throw 'negative fixture unexpectedly passed' }
    $script:checks++
}
$nonce=[byte[]](1,2,3,4,5,6,7,8)
$reply=[Text.Encoding]::ASCII.GetBytes('F17217200001')+$nonce
Assert-UsbHello $reply '17200001' $nonce; $script:checks++
Expect-Failure { Assert-UsbHello $reply '17200002' $nonce }
Expect-Failure { Assert-UsbHello $reply '17200001' ([byte[]](8,7,6,5,4,3,2,1)) }
Expect-Failure { Assert-UsbHello $reply[0..18] '17200001' $nonce }
Expect-Failure { Assert-UsbHello ($reply+[byte]0) '17200001' $nonce }
Expect-Failure { Assert-UsbHello ([Text.Encoding]::ASCII.GetBytes('BOOTSEL')) '17200001' $nonce }
Expect-Failure { Assert-UsbHello $reply 'not-an-id' $nonce }
$app=[pscustomobject]@{kind='app';location='selected';status='OK';serial='port-identity';port='COM14'}
$rom=[pscustomobject]@{kind='rom';location='selected';status='OK';serial='target';port=$null}
$other=[pscustomobject]@{kind='app';location='other';status='OK';serial='port-identity';port='COM9'}
if ((Select-UsbFixtureDevice @($app,$rom,$other) 'app' 'selected' 'target').port -ne 'COM14') { throw 'wrong physical device selected' }; $script:checks++
if (Select-UsbFixtureDevice @($rom) 'rom' 'selected' 'wrong') { throw 'wrong ROM serial accepted' }; $script:checks++
if (Select-UsbFixtureDevice @($rom) 'app' 'selected' 'target') { throw 'ROM was accepted as running firmware' }; $script:checks++
Expect-Failure { Select-UsbFixtureDevice @($app,$app) 'app' 'selected' 'target' }
Expect-Failure { Select-UsbFixtureDevice @($app) 'app' '' 'target' }
$app.port='COM42'
if ((Select-UsbFixtureDevice @($app) 'app' 'selected' 'target').port -ne 'COM42') { throw 'COM renumbering was rejected' }; $script:checks++
$app.status='Error'
if (Select-UsbFixtureDevice @($app) 'app' 'selected' 'target') { throw 'failed enumeration was accepted' }; $script:checks++
Expect-Failure { Invoke-UsbBoundedProcess 'pwsh' @('-NoProfile','-Command','exit 7') 5 }
Expect-Failure { Invoke-UsbBoundedProcess 'pwsh' @('-NoProfile','-Command','Start-Sleep -Seconds 3') 1 }
$large=Invoke-UsbBoundedProcess 'pwsh' @('-NoProfile','-Command','[Console]::Write([string]::new([char]65,100000))') 5
if ($large.Length -ne 100000) { throw 'large process output was truncated or deadlocked' }; $script:checks++
Write-Output "PASS: $script:checks USB host identity, stale-result, and deadline fixtures"

$script:recoveries=0; $script:verifications=0
$recover={ $script:recoveries++; 'recovered-device' }
$verify={ param($device) if ($device -notin @('device','recovered-device')) { throw 'wrong device' }; $script:verifications++ }
$result=Invoke-UsbRecoverableUpdate { 'device' } $recover $verify
if ($result.recovered -or $script:recoveries -ne 0 -or $script:verifications -ne 1) { throw 'healthy USB update used recovery' }; $script:checks++
$result=Invoke-UsbRecoverableUpdate { throw 'transport timeout' } $recover $verify -WarningAction SilentlyContinue
if (-not $result.recovered -or $script:recoveries -ne 1 -or $script:verifications -ne 2) { throw 'USB transport did not recover exactly once' }; $script:checks++
Expect-Failure { Invoke-UsbRecoverableUpdate { 'device' } $recover { throw 'stale result' } }
if ($script:recoveries -ne 1) { throw 'verification failure was hidden by recovery' }; $script:checks++
Expect-Failure { Invoke-UsbRecoverableUpdate { throw 'transport timeout' } { $script:recoveries++; throw 'SWD unavailable' } $verify -WarningAction SilentlyContinue }
if ($script:recoveries -ne 2 -or $script:verifications -ne 2) { throw 'failed recovery was retried or verified' }; $script:checks++
Write-Output "PASS: $script:checks USB fixtures including bounded fallback and fail-closed result verification"

$lockIdentity='fixture-test-'+[Guid]::NewGuid().ToString('N')
$lock=Enter-UsbFixtureLock $lockIdentity
try {
    $child=". '$PSScriptRoot/rp2040-usb-common.ps1'; try { `$lock=Enter-UsbFixtureLock '$($lockIdentity.ToUpperInvariant())'; `$lock.ReleaseMutex(); `$lock.Dispose(); exit 2 } catch { exit 0 }"
    Invoke-UsbBoundedProcess pwsh @('-NoProfile','-Command',$child) 5 | Out-Null
    $script:checks++
} finally { $lock.ReleaseMutex(); $lock.Dispose() }
$lock=Enter-UsbFixtureLock $lockIdentity
$lock.ReleaseMutex(); $lock.Dispose(); $script:checks++
Write-Output "PASS: $script:checks USB fixtures including cross-process exclusive target ownership"
