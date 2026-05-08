#requires -Version 5
<#
.SYNOPSIS
    Wraps cargo with the VS 2022 Build Tools environment.

.DESCRIPTION
    On this machine the auto-detected VS 18 (2026) Community install ships
    only the onecore CRT variants — the regular desktop msvcrt.lib lives in
    the VS 2022 BuildTools install. Sourcing vcvars64.bat from there sets
    VCINSTALLDIR / WindowsSdkDir so rustc and link.exe find the right libs.

.EXAMPLE
    .\scripts\build.ps1 build
    .\scripts\build.ps1 run --release
    .\scripts\build.ps1 check
#>

$ErrorActionPreference = 'Stop'

$vcvars = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if (-not (Test-Path $vcvars)) {
    throw "vcvars64.bat not found at $vcvars. Install VS 2022 Build Tools with the C++ workload."
}

# Source vcvars by running it under cmd and capturing its environment.
cmd.exe /c "`"$vcvars`" >NUL 2>&1 && set" | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') {
        [System.Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process')
    }
}

# Run cargo at the spike's manifest, forwarding all script args.
$manifest = Join-Path $PSScriptRoot '..\Cargo.toml'
& cargo --manifest-path $manifest @args
exit $LASTEXITCODE
