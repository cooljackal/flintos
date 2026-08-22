# SPDX-License-Identifier: Apache-2.0
# Fail while an ARM kernel dependency graph can still reach ESP32/Xtensa crates.

[CmdletBinding()]
param(
    [string]$Target = 'thumbv6m-none-eabi',
    [string]$RootPackage = 'kernel',
    [string]$MetadataFixture,
    [string]$SourceFixture
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot

if ($MetadataFixture) {
    $metadata = Get-Content -Raw -LiteralPath $MetadataFixture | ConvertFrom-Json
} else {
    $raw = & cargo metadata --format-version 1 --filter-platform $Target --no-default-features --features 'kernel/arch-armv6m,kernel/soc-rp2040' --manifest-path (Join-Path $repo 'Cargo.toml') 2>&1
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed: $raw" }
    $metadata = $raw | ConvertFrom-Json
}

$packages = @{}
foreach ($package in $metadata.packages) { $packages[$package.id] = $package }
$nodes = @{}
foreach ($node in $metadata.resolve.nodes) { $nodes[$node.id] = $node }
$roots = @($metadata.packages | Where-Object { $_.name -eq $RootPackage })
if ($roots.Count -ne 1) { throw "expected exactly one package named '$RootPackage', found $($roots.Count)" }

$queue = [System.Collections.Generic.Queue[object]]::new()
$queue.Enqueue([pscustomobject]@{ id=$roots[0].id; path=@($RootPackage) })
$seen = @{}
$leaks = @()
while ($queue.Count) {
    $item = $queue.Dequeue()
    if ($seen[$item.id]) { continue }
    $seen[$item.id] = $true
    foreach ($dependency in $nodes[$item.id].deps) {
        $package = $packages[$dependency.pkg]
        $path = @($item.path) + $package.name
        if ($package.name -match '^(arch-xtensa|soc-esp32|esp32-)') {
            $leaks += ($path -join ' -> ')
        }
        $queue.Enqueue([pscustomobject]@{ id=$dependency.pkg; path=$path })
    }
}

if ($leaks.Count) {
    $leaks | Sort-Object -Unique | ForEach-Object { Write-Error -ErrorAction Continue "ARM dependency leak: $_" }
    exit 1
}

$libPath = if ($SourceFixture) { $SourceFixture } else { Join-Path $repo 'kernel/src/lib.rs' }
$source = Get-Content -Raw -LiteralPath $libPath
$portableModules = @('arch', 'debug', 'dma_broker', 'dynobj', 'heap', 'interrupt', 'mutex', 'queue', 'scheduler', 'smp', 'spawn', 'syscall', 'timer')
foreach ($module in $portableModules) {
    $declaration = "pub mod $module;"
    $offset = $source.IndexOf($declaration, [StringComparison]::Ordinal)
    if ($offset -lt 0) { Write-Error -ErrorAction Continue "portable API missing: $declaration"; $leaks += $declaration; continue }
    $escaped = [regex]::Escape($declaration)
    if ($source -match "(?ms)#\[cfg\([^\]]*feature\s*=\s*`"soc-esp32`"[^\]]*\)\]\s*$escaped") {
        Write-Error -ErrorAction Continue "portable API hidden behind soc-esp32: $declaration"
        $leaks += $declaration
    }
}
$exportMatch = [regex]::Match($source, 'pub\s+use\s+scheduler::\{\s*Scheduler,\s*TaskState,\s*MAX_TASKS\s*\};')
if (-not $exportMatch.Success) {
    Write-Error -ErrorAction Continue 'portable scheduler exports are missing or hidden'
    $leaks += 'scheduler exports'
} else {
    $escaped = [regex]::Escape($exportMatch.Value)
    if ($source -match "(?ms)#\[cfg\([^\]]*feature\s*=\s*`"soc-esp32`"[^\]]*\)\]\s*$escaped") {
        Write-Error -ErrorAction Continue 'portable scheduler exports are hidden behind soc-esp32'
        $leaks += 'scheduler exports'
    }
}
if ($leaks.Count) { exit 1 }
Write-Host "PASS: $RootPackage dependency graph for $Target contains no Xtensa/ESP32 crates."
