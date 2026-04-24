<#
.SYNOPSIS
  Build, package, and (optionally) upload a Velopack release of CodeScope.

.DESCRIPTION
  Steps:
    1. dotnet publish CodeScope.App as a self-contained, framework-dependent-on-runtime
       win-x64 folder build (NOT single-file — Velopack patches loose files for delta updates).
    2. vpk pack — produces:
         releases\CodeScope-win-Setup.exe
         releases\CodeScope-<ver>-full.nupkg
         releases\RELEASES
    3. Optional: vpk upload github — pushes the release artefacts to a GitHub release
       with tag v<ver>.

  Versions are passed in (or default to 0.1.0 for the first release). The vpk tool
  writes delta packages by comparing against any older nupkgs already in the output
  folder, so keep prior releases there for the delta to work.

.PARAMETER Version
  SemVer release version, e.g. 0.1.0. Required.

.PARAMETER Channel
  Velopack channel name. Default: win.

.PARAMETER Publish
  When set, runs `vpk upload github` after packaging.

.PARAMETER GitHubToken
  GitHub PAT with `repo` scope. Falls back to $env:GITHUB_TOKEN.

.PARAMETER RepoUrl
  GitHub repo URL. Default: https://github.com/maui1911/CodeScope

.EXAMPLE
  pwsh tools/release.ps1 -Version 0.1.0
  pwsh tools/release.ps1 -Version 0.1.0 -Publish
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $Version,
    [string] $Channel = 'win',
    [switch] $Publish,
    [string] $GitHubToken = $env:GITHUB_TOKEN,
    [string] $RepoUrl = 'https://github.com/maui1911/CodeScope'
)

$ErrorActionPreference = 'Stop'

$repoRoot   = Resolve-Path (Join-Path $PSScriptRoot '..')
$appProj    = Join-Path $repoRoot 'src\CodeScope.App\CodeScope.App.csproj'
$publishDir = Join-Path $repoRoot 'artifacts\publish'
$releasesDir = Join-Path $repoRoot 'releases'
$icon       = Join-Path $repoRoot 'src\CodeScope.App\assets\codescope.ico'

Write-Host "==> CodeScope release v$Version  (channel=$Channel)" -ForegroundColor Cyan

# 0. Tooling check.
if (-not (Get-Command vpk -ErrorAction SilentlyContinue)) {
    Write-Host "vpk not found — installing globally." -ForegroundColor Yellow
    dotnet tool install -g vpk
    if ($LASTEXITCODE -ne 0) { throw "Failed to install vpk." }
}

# 1. Clean publish folder so vpk doesn't see stale files from a prior run.
if (Test-Path $publishDir) { Remove-Item -Recurse -Force $publishDir }
New-Item -ItemType Directory -Path $publishDir | Out-Null
New-Item -ItemType Directory -Path $releasesDir -Force | Out-Null

# 2. Publish.
#   - self-contained: ship the .NET runtime so users don't need .NET 10 installed.
#   - PublishSingleFile=false: Velopack expects loose files for delta updates.
#   - PublishReadyToRun=true: faster cold start; safe with loose files.
Write-Host "==> dotnet publish" -ForegroundColor Cyan
dotnet publish $appProj `
    -c Release `
    -r win-x64 `
    --self-contained true `
    -p:PublishSingleFile=false `
    -p:PublishReadyToRun=true `
    -p:PublishTrimmed=false `
    -o $publishDir
if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed." }

# 3. Pack.
Write-Host "==> vpk pack" -ForegroundColor Cyan
$packArgs = @(
    'pack',
    '--packId',      'CodeScope',
    '--packVersion', $Version,
    '--packDir',     $publishDir,
    '--mainExe',     'CodeScope.exe',
    '--packTitle',   'CodeScope',
    '--packAuthors', 'maui1911',
    '--icon',        $icon,
    '--channel',     $Channel,
    '--outputDir',   $releasesDir
)
& vpk @packArgs
if ($LASTEXITCODE -ne 0) { throw "vpk pack failed." }

Write-Host "==> Pack complete. Artefacts in $releasesDir" -ForegroundColor Green
Get-ChildItem $releasesDir | Format-Table Name, Length -AutoSize

# 4. Upload (optional).
if ($Publish) {
    if (-not $GitHubToken) {
        throw "Publishing requested but no GitHub token supplied. Pass -GitHubToken or set `$env:GITHUB_TOKEN."
    }
    Write-Host "==> vpk upload github -> $RepoUrl" -ForegroundColor Cyan
    $uploadArgs = @(
        'upload', 'github',
        '--repoUrl',  $RepoUrl,
        '--token',    $GitHubToken,
        '--outputDir', $releasesDir,
        '--channel',  $Channel,
        '--publish',
        '--releaseName', "CodeScope v$Version",
        '--tag',      "v$Version"
    )
    & vpk @uploadArgs
    if ($LASTEXITCODE -ne 0) { throw "vpk upload github failed." }
    Write-Host "==> Release published." -ForegroundColor Green
}
