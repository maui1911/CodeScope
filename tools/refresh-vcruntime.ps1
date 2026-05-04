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

# Architecture safety: the win-x64 release pipeline ships these DLLs as 64-bit
# native binaries. System32 is architecture-dependent:
#   - On 64-bit Windows, %SystemRoot%\System32 holds the *native* arch (x64 or
#     ARM64). On ARM64 Windows it'd hand back ARM64 DLLs with the same name.
#   - From a 32-bit PowerShell on 64-bit Windows, the WoW64 redirector silently
#     remaps System32 → SysWOW64, so we'd pick up x86 DLLs.
# Refuse to run unless the host OS is x64 *and* we're in a 64-bit process. This
# guarantees the DLLs we copy match the win-x64 publish output.
if (-not [Environment]::Is64BitOperatingSystem) {
    throw "This script must run on 64-bit (x64) Windows. Current OS is not 64-bit; the bundled DLLs target win-x64."
}
if (-not [Environment]::Is64BitProcess) {
    throw "This script must run inside a 64-bit PowerShell process. From a 32-bit PowerShell, %SystemRoot%\System32 is redirected to SysWOW64 and would yield x86 DLLs. Re-run from `pwsh.exe` (PowerShell 7+, x64) or `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`."
}
if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw "PROCESSOR_ARCHITECTURE is '$env:PROCESSOR_ARCHITECTURE'; expected 'AMD64'. ARM64 hosts and other architectures are not supported -- the win-x64 release pipeline needs x64 DLLs."
}

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
        throw "Missing $name in $srcDir -- install the Visual C++ Redistributable first (https://aka.ms/vs/17/release/vc_redist.x64.exe)."
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
    # Use .NET directly instead of Get-FileHash so the script works on stripped-down
    # PowerShell hosts where Microsoft.PowerShell.Utility's hash cmdlet is missing.
    $sha  = [System.Security.Cryptography.SHA256]::Create()
    try {
        $stream = [System.IO.File]::OpenRead($path)
        try {
            $bytes = $sha.ComputeHash($stream)
        } finally {
            $stream.Dispose()
        }
    } finally {
        $sha.Dispose()
    }
    $hash = -join ($bytes | ForEach-Object { $_.ToString('x2') })
    Write-Host ("{0,-22} {1,-16} {2}" -f $name, $ver, $hash)
}

Write-Host ""
Write-Host "Update the table in native/vcredist/NOTICE.md if any value changed."
