<#
.SYNOPSIS
    Refresh the bundled Microsoft Visual C++ runtime DLLs from the local System32.

.DESCRIPTION
    Copies vcruntime140.dll, vcruntime140_1.dll, and msvcp140.dll from
    %SystemRoot%\System32 into src/CodeScope.App/native/vcredist/ and prints
    their versions + SHA-256 hashes so the NOTICE.md table can be updated
    by hand.

    Run this on a machine where the latest Microsoft Visual C++ 2015-2022
    Redistributable is installed (any recent Windows dev box qualifies).

.NOTES
    See src/CodeScope.App/native/vcredist/NOTICE.md for context.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$destDir  = Join-Path $repoRoot 'src/CodeScope.App/native/vcredist'
$srcDir   = Join-Path $env:SystemRoot 'System32'
$names    = 'vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll'

if (-not (Test-Path $destDir)) {
    throw "Destination folder not found: $destDir"
}

foreach ($name in $names) {
    $src = Join-Path $srcDir $name
    if (-not (Test-Path $src)) {
        throw "Missing $name in $srcDir — install the Visual C++ Redistributable first (https://aka.ms/vs/17/release/vc_redist.x64.exe)."
    }
    Copy-Item -Path $src -Destination (Join-Path $destDir $name) -Force
}

Write-Host ""
Write-Host "Refreshed VC runtime DLLs in $destDir" -ForegroundColor Green
Write-Host ""
Write-Host ("{0,-22} {1,-16} {2}" -f 'File', 'Version', 'SHA-256')
Write-Host ("{0,-22} {1,-16} {2}" -f ('-' * 20), ('-' * 14), ('-' * 64))

foreach ($name in $names) {
    $path = Join-Path $destDir $name
    $ver  = (Get-Item $path).VersionInfo.FileVersion
    $hash = (Get-FileHash -Algorithm SHA256 -Path $path).Hash.ToLowerInvariant()
    Write-Host ("{0,-22} {1,-16} {2}" -f $name, $ver, $hash)
}

Write-Host ""
Write-Host "Update the table in native/vcredist/NOTICE.md if any value changed."
