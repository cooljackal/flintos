# SPDX-License-Identifier: Apache-2.0
# Child process so a hung mass-storage write has a killable deadline.
param([Parameter(Mandatory)][string]$Source,[Parameter(Mandatory)][string]$Destination)
$ErrorActionPreference='Stop'
if ($Destination -notmatch '^[A-Za-z]:\\$' -or
    (Get-Volume -DriveLetter $Destination[0]).FileSystemLabel -ne 'RPI-RP2') { throw 'destination is not an RPI-RP2 volume root' }
Copy-Item -LiteralPath $Source -Destination (Join-Path $Destination 'flint-usb.uf2')
