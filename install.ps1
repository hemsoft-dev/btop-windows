<#
.SYNOPSIS
    Installs or uninstalls btop for Windows (btop.exe) from GitHub Releases
    with SHA-256 verification and user-level PATH setup (no Administrator required).

.EXAMPLE
    irm https://raw.githubusercontent.com/hemsoft-dev/btop-windows/main/install.ps1 | iex

.EXAMPLE
    .\install.ps1 -Version v0.1.0

.EXAMPLE
    .\install.ps1 -Uninstall
#>
[CmdletBinding()]
param(
    [string]$Version = '',
    [string]$InstallDir = '',
    [switch]$NoPath,
    [switch]$Uninstall,
    [string]$LocalArchive = '',
    [string]$ChecksumFile = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = 'hemsoft-dev/btop-windows'

function Assert-WindowsX64 {
    if ($env:OS -ne 'Windows_NT') {
        throw 'btop-windows supports Windows 11 x64 only.'
    }
    if (-not [Environment]::Is64BitOperatingSystem) {
        throw 'btop-windows requires 64-bit (x86_64) Windows 11.'
    }
    $arch = $env:PROCESSOR_ARCHITECTURE
    $wowArch = $env:PROCESSOR_ARCHITEW6432
    if ($arch -ne 'AMD64' -and $wowArch -ne 'AMD64') {
        throw "btop-windows v1 targets x86_64 (AMD64); detected architecture '$arch'."
    }
}

function Normalize-PathEntry {
    param([string]$PathValue)
    if ([string]::IsNullOrWhiteSpace($PathValue)) {
        return ''
    }
    return $PathValue.Trim().TrimEnd('\')
}

function Add-UserPathEntry {
    param([string]$Directory)
    $normalizedTarget = Normalize-PathEntry -PathValue $Directory
    $currentUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @()
    if (-not [string]::IsNullOrWhiteSpace($currentUserPath)) {
        $entries = @($currentUserPath -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    }

    $alreadyPresent = $false
    foreach ($entry in $entries) {
        if ((Normalize-PathEntry -PathValue $entry) -ieq $normalizedTarget) {
            $alreadyPresent = $true
            break
        }
    }

    if (-not $alreadyPresent) {
        $updatedEntries = @($entries + $Directory)
        [Environment]::SetEnvironmentVariable('Path', ($updatedEntries -join ';'), 'User')
        Write-Host "Added '$Directory' to User PATH."
    }

    $sessionEntries = @($env:PATH -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $inSession = $false
    foreach ($entry in $sessionEntries) {
        if ((Normalize-PathEntry -PathValue $entry) -ieq $normalizedTarget) {
            $inSession = $true
            break
        }
    }
    if (-not $inSession) {
        $env:PATH = "$Directory;$env:PATH"
    }
}

function Remove-UserPathEntry {
    param([string]$Directory)
    $normalizedTarget = Normalize-PathEntry -PathValue $Directory
    $currentUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ([string]::IsNullOrWhiteSpace($currentUserPath)) {
        return
    }
    $kept = @()
    $removed = $false
    foreach ($entry in ($currentUserPath -split ';')) {
        if ([string]::IsNullOrWhiteSpace($entry)) {
            continue
        }
        if ((Normalize-PathEntry -PathValue $entry) -ieq $normalizedTarget) {
            $removed = $true
            continue
        }
        $kept += $entry
    }
    if ($removed) {
        [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
        Write-Host "Removed '$Directory' from User PATH."
    }
}

function Assert-ChecksumMatch {
    param(
        [string]$ArchivePath,
        [string]$SumsPath,
        [string]$ExpectedArchiveName
    )
    $expectedHash = $null
    foreach ($line in (Get-Content -LiteralPath $SumsPath)) {
        if ($line -match '^\s*([0-9a-fA-F]{64})\s+\*?(.+?)\s*$') {
            $fileName = [IO.Path]::GetFileName($Matches[2].Trim())
            if ($fileName -ieq $ExpectedArchiveName) {
                $expectedHash = $Matches[1].ToLowerInvariant()
                break
            }
        }
    }
    if (-not $expectedHash) {
        throw "SHA256SUMS.txt does not contain an entry for '$ExpectedArchiveName'."
    }
    $actualHash = (Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $expectedHash) {
        throw "SHA-256 mismatch for '$ExpectedArchiveName': expected $expectedHash, got $actualHash."
    }
    Write-Host "Verified SHA-256 checksum ($actualHash)."
}

Assert-WindowsX64

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        throw 'LOCALAPPDATA environment variable is not set; pass -InstallDir explicitly.'
    }
    $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\btop'
}
$InstallDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($InstallDir)

if ($Uninstall) {
    if (Test-Path -LiteralPath $InstallDir -PathType Container) {
        foreach ($item in @('btop.exe', 'LICENSE', 'NOTICE', 'README.md')) {
            $p = Join-Path $InstallDir $item
            if (Test-Path -LiteralPath $p -PathType Leaf) {
                Remove-Item -LiteralPath $p -Force
            }
        }
        $themesPath = Join-Path $InstallDir 'themes'
        if (Test-Path -LiteralPath $themesPath -PathType Container) {
            Remove-Item -LiteralPath $themesPath -Recurse -Force
        }
        if ((Get-ChildItem -LiteralPath $InstallDir -Force | Measure-Object).Count -eq 0) {
            Remove-Item -LiteralPath $InstallDir -Force
        }
        Write-Host "Uninstalled btop from '$InstallDir'."
    }
    else {
        Write-Host "Nothing to uninstall at '$InstallDir'."
    }
    Remove-UserPathEntry -Directory $InstallDir
    if (-not [string]::IsNullOrWhiteSpace($env:APPDATA)) {
        $cfg = Join-Path $env:APPDATA 'btop\btop.conf'
        if (Test-Path -LiteralPath $cfg -PathType Leaf) {
            Write-Host "User config preserved at '$cfg'."
        }
    }
    return
}

if (-not [string]::IsNullOrWhiteSpace($Version) -and ($Version -notmatch '^v[0-9A-Za-z._-]+$')) {
    throw "Invalid -Version '$Version'. Expected format such as 'v0.1.0'."
}

$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("btop-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempRoot -Force | Out-Null

try {
    $archivePath = $null
    $archiveName = $null

    if (-not [string]::IsNullOrWhiteSpace($LocalArchive)) {
        if (-not (Test-Path -LiteralPath $LocalArchive -PathType Leaf)) {
            throw "Local archive not found: '$LocalArchive'."
        }
        $archivePath = (Resolve-Path -LiteralPath $LocalArchive).Path
        $archiveName = [IO.Path]::GetFileName($archivePath)
        if (-not [string]::IsNullOrWhiteSpace($ChecksumFile)) {
            if (-not (Test-Path -LiteralPath $ChecksumFile -PathType Leaf)) {
                throw "Checksum file not found: '$ChecksumFile'."
            }
            Assert-ChecksumMatch -ArchivePath $archivePath -SumsPath (Resolve-Path -LiteralPath $ChecksumFile).Path -ExpectedArchiveName $archiveName
        }
    }
    else {
        if ($PSVersionTable.PSVersion.Major -lt 6) {
            [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        }
        $headers = @{
            'User-Agent' = 'btop-windows-installer'
            'Accept'     = 'application/vnd.github+json'
        }
        $releaseUrl = if ([string]::IsNullOrWhiteSpace($Version)) {
            "https://api.github.com/repos/$repo/releases/latest"
        }
        else {
            "https://api.github.com/repos/$repo/releases/tags/$Version"
        }

        Write-Host "Fetching release metadata from $releaseUrl..."
        $release = Invoke-RestMethod -Uri $releaseUrl -Headers $headers
        $tag = [string]$release.tag_name
        if ([string]::IsNullOrWhiteSpace($tag) -or ($tag -notmatch '^v[0-9A-Za-z._-]+$')) {
            throw "Unexpected release tag '$tag' from GitHub API."
        }

        $archiveName = "btop-windows-$tag-x86_64-windows.zip"
        $zipAsset = $release.assets | Where-Object { $_.name -eq $archiveName } | Select-Object -First 1
        $sumAsset = $release.assets | Where-Object { $_.name -eq 'SHA256SUMS.txt' } | Select-Object -First 1
        if (-not $zipAsset) {
            throw "Release '$tag' does not include expected asset '$archiveName'."
        }
        if (-not $sumAsset) {
            throw "Release '$tag' is missing SHA256SUMS.txt; refusing unverified download."
        }

        foreach ($assetUrl in @([string]$zipAsset.browser_download_url, [string]$sumAsset.browser_download_url)) {
            if ($assetUrl -notmatch '^https://github\.com/hemsoft-dev/btop-windows/releases/download/') {
                throw "Refusing unexpected asset download URL: $assetUrl"
            }
        }

        $archivePath = Join-Path $tempRoot $archiveName
        $sumsPath = Join-Path $tempRoot 'SHA256SUMS.txt'

        Write-Host "Downloading $archiveName ($tag)..."
        Invoke-WebRequest -Uri $zipAsset.browser_download_url -Headers $headers -OutFile $archivePath
        Invoke-WebRequest -Uri $sumAsset.browser_download_url -Headers $headers -OutFile $sumsPath
        Assert-ChecksumMatch -ArchivePath $archivePath -SumsPath $sumsPath -ExpectedArchiveName $archiveName
    }

    $stageDir = Join-Path $tempRoot 'extracted'
    Expand-Archive -LiteralPath $archivePath -DestinationPath $stageDir -Force

    $stagedExe = Join-Path $stageDir 'btop.exe'
    if (-not (Test-Path -LiteralPath $stagedExe -PathType Leaf)) {
        throw "Archive '$archiveName' does not contain btop.exe."
    }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item -LiteralPath $stagedExe -Destination (Join-Path $InstallDir 'btop.exe') -Force
    foreach ($doc in @('LICENSE', 'NOTICE', 'README.md')) {
        $srcDoc = Join-Path $stageDir $doc
        if (Test-Path -LiteralPath $srcDoc -PathType Leaf) {
            Copy-Item -LiteralPath $srcDoc -Destination (Join-Path $InstallDir $doc) -Force
        }
    }
    $stagedThemes = Join-Path $stageDir 'themes'
    if (Test-Path -LiteralPath $stagedThemes -PathType Container) {
        $destThemes = Join-Path $InstallDir 'themes'
        New-Item -ItemType Directory -Path $destThemes -Force | Out-Null
        Copy-Item -Path (Join-Path $stagedThemes '*.theme') -Destination $destThemes -Force
    }

    if (-not $NoPath) {
        Add-UserPathEntry -Directory $InstallDir
    }

    $installedExe = Join-Path $InstallDir 'btop.exe'
    $verOut = & $installedExe --version
    if ($LASTEXITCODE -ne 0) {
        throw "Installed binary failed verification (--version exited $LASTEXITCODE)."
    }

    Write-Host "Installed $verOut to '$installedExe'."
    Write-Host "Run 'btop' in Windows Terminal to launch."
}
finally {
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
