#requires -Version 5
$ErrorActionPreference = 'Continue'

Write-Host "=== Visual Studio installs ==="
$vswhere = "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe"
if (Test-Path $vswhere) {
    & $vswhere -all -products * -format json | ConvertFrom-Json | ForEach-Object {
        Write-Host ("  [{0}] {1}" -f $_.displayName, $_.installationPath)
        $libRoot = Join-Path $_.installationPath 'VC\Tools\MSVC'
        if (Test-Path $libRoot) {
            Get-ChildItem $libRoot -Directory | ForEach-Object {
                $msvcrt = Join-Path $_.FullName 'lib\x64\msvcrt.lib'
                Write-Host ("    MSVC {0} : msvcrt.lib? {1}" -f $_.Name, (Test-Path $msvcrt))
            }
        } else {
            Write-Host "    (no VC\Tools\MSVC dir)"
        }
    }
} else {
    Write-Host "vswhere not found"
}

Write-Host ""
Write-Host "=== Windows SDK Lib versions ==="
$sdkLib = "C:\Program Files (x86)\Windows Kits\10\Lib"
if (Test-Path $sdkLib) {
    Get-ChildItem $sdkLib -Directory | ForEach-Object {
        $um = Join-Path $_.FullName 'um\x64\kernel32.lib'
        $ucrt = Join-Path $_.FullName 'ucrt\x64\ucrt.lib'
        Write-Host ("  SDK {0} : kernel32.lib? {1}  ucrt.lib? {2}" -f $_.Name, (Test-Path $um), (Test-Path $ucrt))
    }
} else {
    Write-Host "  (no Lib folder under Windows Kits\10)"
}

Write-Host ""
Write-Host "=== Looking for msvcrt.lib anywhere ==="
$found = @()
$searchRoots = @(
    'C:\Program Files\Microsoft Visual Studio',
    'C:\Program Files (x86)\Microsoft Visual Studio',
    'C:\Program Files (x86)\Windows Kits'
)
foreach ($root in $searchRoots) {
    if (Test-Path $root) {
        $found += Get-ChildItem -Path $root -Recurse -Filter 'msvcrt.lib' -ErrorAction SilentlyContinue |
                  Select-Object -First 10 -ExpandProperty FullName
    }
}
if ($found.Count -eq 0) {
    Write-Host "  msvcrt.lib NOT FOUND on this system"
} else {
    $found | ForEach-Object { Write-Host "  $_" }
}
