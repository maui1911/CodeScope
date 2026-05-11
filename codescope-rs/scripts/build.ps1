#requires -Version 5
<#
.SYNOPSIS
    Wraps cargo with a Visual Studio x64 MSVC environment.

.DESCRIPTION
    Locates vcvars64.bat via CODESCOPE_VCVARS64, VSINSTALLDIR, vswhere, or
    common Visual Studio install paths. Sourcing vcvars64.bat sets VCINSTALLDIR
    / WindowsSdkDir so rustc and link.exe find the right desktop MSVC libs.

.EXAMPLE
    .\scripts\build.ps1 build
    .\scripts\build.ps1 run --release
    .\scripts\build.ps1 check
#>

$ErrorActionPreference = 'Stop'

function Resolve-VcVars64 {
    if ($env:CODESCOPE_VCVARS64) {
        if (Test-Path $env:CODESCOPE_VCVARS64) {
            return $env:CODESCOPE_VCVARS64
        }

        throw "CODESCOPE_VCVARS64 points to '$env:CODESCOPE_VCVARS64', but that file does not exist."
    }

    $candidates = [System.Collections.Generic.List[string]]::new()

    if ($env:VSINSTALLDIR) {
        $candidates.Add((Join-Path $env:VSINSTALLDIR 'VC\Auxiliary\Build\vcvars64.bat'))
    }

    $programFilesX86 = [Environment]::GetFolderPath('ProgramFilesX86')
    $vswhere = Join-Path $programFilesX86 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        $installations = @(& $vswhere -products * `
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
            -property installationPath `
            -format value)
        if (-not $installations) {
            $installations = @(& $vswhere -all -products * -property installationPath -format value)
        }

        foreach ($installation in $installations) {
            if ([string]::IsNullOrWhiteSpace($installation)) {
                continue
            }

            $candidates.Add((Join-Path $installation 'VC\Auxiliary\Build\vcvars64.bat'))
        }
    }

    $commonRoots = @(
        'C:\Program Files\Microsoft Visual Studio\18\Professional',
        'C:\Program Files\Microsoft Visual Studio\18\Enterprise',
        'C:\Program Files\Microsoft Visual Studio\18\Community',
        'C:\Program Files\Microsoft Visual Studio\18\BuildTools',
        'C:\Program Files\Microsoft Visual Studio\2022\Professional',
        'C:\Program Files\Microsoft Visual Studio\2022\Enterprise',
        'C:\Program Files\Microsoft Visual Studio\2022\Community',
        'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools'
    )

    foreach ($root in $commonRoots) {
        $candidates.Add((Join-Path $root 'VC\Auxiliary\Build\vcvars64.bat'))
    }

    foreach ($candidate in ($candidates | Select-Object -Unique)) {
        if (Test-Path $candidate) {
            return $candidate
        }
    }

    throw "vcvars64.bat not found. Install Visual Studio/Build Tools with the 'Desktop development with C++' workload, or set CODESCOPE_VCVARS64 to the full vcvars64.bat path. Checked: $($candidates -join '; ')"
}

$vcvars = Resolve-VcVars64
Write-Host "Using MSVC environment: $vcvars"

# Source vcvars by running it under cmd and capturing its environment.
cmd.exe /c "`"$vcvars`" >NUL 2>&1 && set" | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') {
        [System.Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process')
    }
}

# Run cargo from the spike root, forwarding all script args verbatim.
# Changing directory keeps zero-arg, global-option, and `+toolchain` invocations valid while still
# targeting codescope-rs.
$manifestDir = Resolve-Path (Join-Path $PSScriptRoot '..')
Push-Location $manifestDir
try {
    & cargo @args
    $exitCode = $LASTEXITCODE
} finally {
    Pop-Location
}
exit $exitCode
