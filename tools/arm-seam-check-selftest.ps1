# SPDX-License-Identifier: Apache-2.0
# Fixture tests for tools/arm-seam-check.ps1.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$check = Join-Path $PSScriptRoot 'arm-seam-check.ps1'
$work = Join-Path $root 'target/tmp/arm-seam-check-tests'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$failures = 0

$modules = @('arch', 'debug', 'dma_broker', 'dynobj', 'heap', 'interrupt', 'mutex', 'queue', 'scheduler', 'smp', 'spawn', 'syscall', 'timer')
$cleanSource = (($modules | ForEach-Object { "pub mod $_;" }) -join "`n") + "`npub use scheduler::{Scheduler, TaskState, MAX_TASKS};`n"

function Invoke-Case {
    param([string]$Name, [object]$Metadata, [string]$Source, [int]$Want, [string]$Pattern)
    $metadataPath = Join-Path $work "$Name-metadata.json"
    $sourcePath = Join-Path $work "$Name-lib.rs"
    ConvertTo-Json -InputObject $Metadata -Depth 12 | Set-Content -LiteralPath $metadataPath
    Set-Content -LiteralPath $sourcePath -Value $Source
    $output = & pwsh -NoProfile -File $check -MetadataFixture $metadataPath -SourceFixture $sourcePath 2>&1 | Out-String
    $actual = $LASTEXITCODE
    if ($actual -eq $Want -and $output -match $Pattern) { Write-Host "  ok    $Name"; return }
    Write-Host "  FAIL  $Name (exit $actual, wanted $Want): $output"
    $script:failures++
}

function Metadata {
    param([object[]]$ExtraPackages=@(), [object[]]$KernelDeps=@(), [object[]]$ExtraNodes=@())
    @{
        packages = @(@{ id='kernel-id'; name='kernel' }) + $ExtraPackages
        resolve = @{ nodes = @(@{ id='kernel-id'; deps=$KernelDeps }) + $ExtraNodes }
    }
}

Invoke-Case 'clean' (Metadata) $cleanSource 0 'PASS:'
Invoke-Case 'direct-leak' (Metadata @(@{id='xtensa-id';name='arch-xtensa'}) @(@{pkg='xtensa-id'}) @(@{id='xtensa-id';deps=@()})) $cleanSource 1 'kernel -> arch-xtensa'
Invoke-Case 'transitive-leak' (Metadata @(@{id='board-id';name='board'},@{id='soc-id';name='soc-esp32'}) @(@{pkg='board-id'}) @(@{id='board-id';deps=@(@{pkg='soc-id'})},@{id='soc-id';deps=@()})) $cleanSource 1 'kernel -> board -> soc-esp32'
$hiddenSource = $cleanSource.Replace('pub mod queue;', "#[cfg(feature = `"soc-esp32`")]`npub mod queue;")
Invoke-Case 'hidden-portable-module' (Metadata) $hiddenSource 1 'portable API hidden behind soc-esp32: pub mod queue'
Invoke-Case 'missing-scheduler-export' (Metadata) ($cleanSource -replace 'pub use scheduler::.*', '') 1 'scheduler exports are missing or hidden'
$hiddenExport = $cleanSource.Replace('pub use scheduler::{Scheduler, TaskState, MAX_TASKS};', "#[cfg(feature = `"soc-esp32`")]`npub use scheduler::{Scheduler, TaskState, MAX_TASKS};")
Invoke-Case 'hidden-scheduler-export' (Metadata) $hiddenExport 1 'scheduler exports are hidden behind soc-esp32'

Remove-Item -Recurse -Force -LiteralPath $work
if ($failures) { throw "$failures ARM seam check self-test(s) failed" }
Write-Host 'All ARM seam check self-tests passed.'
