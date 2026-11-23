#Requires -Version 7.0
<#
.SYNOPSIS
    Audits a Windows x86_64 PE executable to confirm static CRT linkage and
    ensure only allowlisted Windows system DLLs appear in its import table.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$BinaryPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) {
    throw "Binary not found: $BinaryPath"
}

$resolvedPath = (Resolve-Path -LiteralPath $BinaryPath).Path
$fileInfo = Get-Item -LiteralPath $resolvedPath

# Verify PE32+ x86_64 (AMD64 = 0x8664) header directly.
$bytes = [System.IO.File]::ReadAllBytes($resolvedPath)
if ($bytes.Length -lt 0x44 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
    throw "File is not a valid PE binary (missing MZ header): $resolvedPath"
}
$peOffset = [BitConverter]::ToInt32($bytes, 0x3C)
if ($peOffset -lt 0 -or ($peOffset + 6) -gt $bytes.Length) {
    throw "Invalid PE header offset ($peOffset) in $resolvedPath"
}
if ($bytes[$peOffset] -ne 0x50 -or $bytes[$peOffset + 1] -ne 0x45 -or $bytes[$peOffset + 2] -ne 0 -or $bytes[$peOffset + 3] -ne 0) {
    throw "Missing PE signature in $resolvedPath"
}
$machine = [BitConverter]::ToUInt16($bytes, $peOffset + 4)
if ($machine -ne 0x8664) {
    throw ("Expected x86_64 PE machine 0x8664, found 0x{0:X4} in {1}" -f $machine, $resolvedPath)
}

function Find-Dumpbin {
    $cmd = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path -LiteralPath $vswhere -PathType Leaf) {
        $installPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if ($LASTEXITCODE -eq 0 -and -not [string]::IsNullOrWhiteSpace($installPath)) {
            $candidate = Get-ChildItem -Path (Join-Path $installPath 'VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe') -ErrorAction SilentlyContinue |
                Sort-Object FullName -Descending |
                Select-Object -First 1
            if ($candidate) {
                return $candidate.FullName
            }
        }
    }
    return $null
}

function Get-ImportsWithDumpbin {
    param([string]$DumpbinPath, [string]$ExePath)
    $output = & $DumpbinPath /dependents $ExePath 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "dumpbin /dependents failed (exit $LASTEXITCODE):`n$($output -join "`n")"
    }
    $dlls = [System.Collections.Generic.List[string]]::new()
    $inDeps = $false
    foreach ($line in $output) {
        $text = [string]$line
        if ($text -match '^\s*Image has the following (delay load )?dependencies:\s*$') {
            $inDeps = $true
            continue
        }
        if ($inDeps) {
            if ($text -match '^\s*Summary\s*$') {
                $inDeps = $false
                continue
            }
            if ($text -match '^\s*([A-Za-z0-9._-]+\.dll)\s*$') {
                $dlls.Add($Matches[1].ToLowerInvariant())
            }
        }
    }
    return $dlls
}

function Get-ImportsWithLlvmReadobj {
    param([string]$ReadobjPath, [string]$ExePath)
    $output = & $ReadobjPath --coff-imports $ExePath 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "llvm-readobj --coff-imports failed (exit $LASTEXITCODE):`n$($output -join "`n")"
    }
    $dlls = [System.Collections.Generic.List[string]]::new()
    foreach ($line in $output) {
        $text = [string]$line
        if ($text -match '^\s*Name:\s*([A-Za-z0-9._-]+\.dll)\s*$') {
            $dlls.Add($Matches[1].ToLowerInvariant())
        }
    }
    return $dlls
}

$dumpbin = Find-Dumpbin
$llvmReadobj = Get-Command llvm-readobj -ErrorAction SilentlyContinue

if ($dumpbin) {
    Write-Host "[pe-audit] Tool: $dumpbin /dependents"
    $rawImports = Get-ImportsWithDumpbin -DumpbinPath $dumpbin -ExePath $resolvedPath
} elseif ($llvmReadobj) {
    Write-Host "[pe-audit] Tool: $($llvmReadobj.Source) --coff-imports"
    $rawImports = Get-ImportsWithLlvmReadobj -ReadobjPath $llvmReadobj.Source -ExePath $resolvedPath
} else {
    throw "Neither dumpbin.exe (MSVC Build Tools) nor llvm-readobj was found on this host."
}

$imports = @($rawImports | Sort-Object -Unique)
if ($imports.Count -eq 0) {
    throw "PE import parser returned zero DLLs for $resolvedPath; refusing to pass an empty audit."
}

Write-Host ("[pe-audit] Binary: {0} ({1:N0} bytes, x86_64 PE32+)" -f $resolvedPath, $fileInfo.Length)
Write-Host ("[pe-audit] Imported DLLs ({0}): {1}" -f $imports.Count, ($imports -join ', '))

# Explicit denylist for MSVC/UCRT dynamic redistributable runtimes.
$forbiddenPatterns = @(
    '^vcruntime',
    '^msvcp',
    '^msvcr',
    '^concrt',
    '^vccorlib',
    '^ucrtbase',
    '^api-ms-win-crt-'
)

# Allowlisted inbox Windows 11 system DLLs.
$allowedSystemDlls = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
@(
    'advapi32.dll',
    'bcrypt.dll',
    'bcryptprimitives.dll',
    'crypt32.dll',
    'dxgi.dll',
    'gdi32.dll',
    'iphlpapi.dll',
    'kernel32.dll',
    'kernelbase.dll',
    'ntdll.dll',
    'ole32.dll',
    'oleaut32.dll',
    'pdh.dll',
    'powrprof.dll',
    'psapi.dll',
    'rpcrt4.dll',
    'secur32.dll',
    'shell32.dll',
    'user32.dll',
    'winmm.dll',
    'ws2_32.dll'
) | ForEach-Object { [void]$allowedSystemDlls.Add($_) }

$violations = [System.Collections.Generic.List[string]]::new()
foreach ($dll in $imports) {
    $matchedForbidden = $false
    foreach ($pat in $forbiddenPatterns) {
        if ($dll -match $pat) {
            $violations.Add("Forbidden dynamic CRT/runtime DLL imported: $dll")
            $matchedForbidden = $true
            break
        }
    }
    if ($matchedForbidden) {
        continue
    }
    if (-not $allowedSystemDlls.Contains($dll) -and ($dll -notmatch '^api-ms-win-core-[a-z0-9-]+\.dll$')) {
        $violations.Add("Unexpected non-system DLL imported: $dll")
    }
}

if ($violations.Count -gt 0) {
    foreach ($v in $violations) {
        Write-Error $v
    }
    throw "PE import audit failed with $($violations.Count) violation(s)."
}

Write-Host "[pe-audit] PASS: static CRT confirmed; only allowlisted Windows system DLLs imported."
