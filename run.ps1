<#
.SYNOPSIS
    Builds (when needed) and launches btop-windows from the repository root.

.EXAMPLE
    .\run.ps1
    Launches the release binary with the repository themes directory.

.EXAMPLE
    .\run.ps1 -Build
    Forces a fresh release build before launching.

.EXAMPLE
    .\run.ps1 -DebugBuild
    Builds and launches the faster debug binary.

.EXAMPLE
    .\run.ps1 -Replay
    Launches against the representative 16-core dual-GPU replay fixture.

.EXAMPLE
    .\run.ps1 --update-ms 1000 --preset 2
    Forwards arbitrary CLI flags directly to btop.exe.
#>
[CmdletBinding()]
param(
    [switch]$Build,
    [switch]$NoBuild,
    [switch]$DebugBuild,
    [switch]$Replay,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$AppArgs
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = $PSScriptRoot
$targetTriple = 'x86_64-pc-windows-msvc'
$profileDir = if ($DebugBuild) { 'debug' } else { 'release' }
$exePath = Join-Path $repoRoot "target\$targetTriple\$profileDir\btop.exe"
$themesDir = Join-Path $repoRoot 'themes'
$defaultFixture = Join-Path $repoRoot 'fixtures\win11-16core-dual-gpu-desktop.jsonl'

function Test-RebuildNeeded {
    param([string]$BinaryPath)

    if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) {
        return $true
    }

    $binaryTime = (Get-Item -LiteralPath $BinaryPath).LastWriteTimeUtc
    $sourceFiles = @()
    foreach ($topFile in @('Cargo.toml', 'Cargo.lock', '.cargo\config.toml')) {
        $full = Join-Path $repoRoot $topFile
        if (Test-Path -LiteralPath $full -PathType Leaf) {
            $sourceFiles += Get-Item -LiteralPath $full
        }
    }
    foreach ($dir in @('apps', 'crates')) {
        $fullDir = Join-Path $repoRoot $dir
        if (Test-Path -LiteralPath $fullDir -PathType Container) {
            $sourceFiles += Get-ChildItem -LiteralPath $fullDir -Recurse -File -Include '*.rs', 'Cargo.toml'
        }
    }
    if (Test-Path -LiteralPath $themesDir -PathType Container) {
        $sourceFiles += Get-ChildItem -LiteralPath $themesDir -File -Filter '*.theme'
    }

    foreach ($file in $sourceFiles) {
        if ($file.LastWriteTimeUtc -gt $binaryTime) {
            return $true
        }
    }
    return $false
}

function Invoke-CargoBuild {
    $cargoArgs = @('build', '--locked', '-p', 'btop-windows', '--target', $targetTriple)
    if (-not $DebugBuild) {
        $cargoArgs += '--release'
    }

    $rustup = Get-Command rustup -ErrorAction SilentlyContinue
    Push-Location -LiteralPath $repoRoot
    try {
        if ($rustup) {
            & $rustup.Source run "stable-$targetTriple" cargo @cargoArgs
        }
        else {
            & cargo @cargoArgs
        }
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
    }
    finally {
        Pop-Location
    }
}

if ($Build -and $NoBuild) {
    throw 'Cannot combine -Build and -NoBuild.'
}

$needsBuild = if ($NoBuild) {
    $false
}
elseif ($Build) {
    $true
}
else {
    Test-RebuildNeeded -BinaryPath $exePath
}

if ($needsBuild) {
    Write-Host "Building btop-windows ($profileDir, $targetTriple)..." -ForegroundColor Cyan
    Invoke-CargoBuild
}

if (-not (Test-Path -LiteralPath $exePath -PathType Leaf)) {
    throw "Binary not found at '$exePath'. Run .\run.ps1 -Build to compile it."
}

$launchArgs = [System.Collections.Generic.List[string]]::new()
$forwarded = @($AppArgs | Where-Object { $null -ne $_ })

$isInfoFlag = $false
$hasThemesDir = $false
$hasReplay = $false
foreach ($arg in $forwarded) {
    if ($arg -in @('-h', '--help', '-V', '--version', '--default-config')) {
        $isInfoFlag = $true
    }
    if ($arg -eq '--themes-dir' -or $arg -like '--themes-dir=*') {
        $hasThemesDir = $true
    }
    if ($arg -eq '--replay' -or $arg -like '--replay=*') {
        $hasReplay = $true
    }
}

if (-not $isInfoFlag -and -not $hasThemesDir -and (Test-Path -LiteralPath $themesDir -PathType Container)) {
    $launchArgs.Add('--themes-dir')
    $launchArgs.Add($themesDir)
}

if ($Replay -and -not $hasReplay) {
    $launchArgs.Add('--replay')
    $launchArgs.Add($defaultFixture)
}

foreach ($arg in $forwarded) {
    $launchArgs.Add($arg)
}

& $exePath @launchArgs
exit $LASTEXITCODE
