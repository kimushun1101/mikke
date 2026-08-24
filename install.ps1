# Windows installer for mikke. Downloads the release zip, verifies it against
# SHA256SUMS, and places mikke.exe.
#
#   irm https://raw.githubusercontent.com/kimushun1101/mikke/main/install.ps1 | iex
#
# Options are given via environment variables (parameters cannot be passed through iex):
#   $env:MIKKE_VARIANT     = "slim"      # full (default) / slim
#   $env:MIKKE_VERSION     = "v0.2.0"    # default: latest
#   $env:MIKKE_INSTALL_DIR = "C:/tools"  # default: $env:LOCALAPPDATA/Programs/mikke
# MIKKE_TARGET is not provided because there is only one Windows target.
#
# Use install.sh on Linux / macOS.
#
# ENCODING CONSTRAINT (#44): keep this file ASCII-only and without a UTF-8 BOM.
# - With a BOM, `irm | iex` hands the script to the parser as a string whose
#   first token is U+FEFF glued to whatever follows (`<BOM>#Requires`,
#   `<BOM>if`, ...), which fails with CommandNotFoundException on every run.
# - Without a BOM, Windows PowerShell 5.1 reads the file as ANSI (cp932 on
#   Japanese Windows) when run as a file; non-ASCII text can then swallow the
#   following quote, brace or newline and break parsing.
# CI enforces both (see .github/workflows/ci.yml).

# `#Requires -Version 5` is ignored when the script is fed through iex, so check explicitly.
if ($PSVersionTable.PSVersion.Major -lt 5) {
    throw "mikke installer requires PowerShell 5 or later (running: $($PSVersionTable.PSVersion))"
}

$ErrorActionPreference = "Stop"

$Repo = "kimushun1101/mikke"
$Variant = if ($env:MIKKE_VARIANT) { $env:MIKKE_VARIANT } else { "full" }
$Version = if ($env:MIKKE_VERSION) { $env:MIKKE_VERSION } else { "latest" }
$InstallDir = if ($env:MIKKE_INSTALL_DIR) { $env:MIKKE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\mikke" }

if (@("slim", "full") -notcontains $Variant) { throw "MIKKE_VARIANT must be slim or full (got: $Variant)" }
if ($Version -cmatch "^[0-9]+\.[0-9]+\.[0-9]+$") { $Version = "v$Version" }
if ($Version -cne "latest" -and $Version -cnotmatch "^v[0-9]+\.[0-9]+\.[0-9]+$") {
    throw "MIKKE_VERSION must be latest or v<major>.<minor>.<patch> (got: $Version)"
}

function Invoke-WithoutProgress {
    param([scriptblock]$Request)

    $PreviousProgressPreference = $ProgressPreference
    try {
        $ProgressPreference = "SilentlyContinue"
        & $Request
    }
    finally {
        $ProgressPreference = $PreviousProgressPreference
    }
}

function ConvertTo-NormalizedPath {
    param([string]$Path)

    if ([string]::IsNullOrWhiteSpace($Path)) { return $null }
    try {
        $FullPath = [System.IO.Path]::GetFullPath($Path)
        $Root = [System.IO.Path]::GetPathRoot($FullPath)
        if ($FullPath.Length -gt $Root.Length) { $FullPath = $FullPath.TrimEnd("\", "/") }
        return $FullPath
    }
    catch {
        return $null
    }
}

$InstallDir = ConvertTo-NormalizedPath $InstallDir
if (-not $InstallDir) { throw "MIKKE_INSTALL_DIR is invalid (got: $env:MIKKE_INSTALL_DIR)" }

# Detect the CPU (only x86_64 is distributed for Windows). The WOW64 variable lets a
# 32-bit PowerShell on a 64-bit OS be detected as AMD64.
$Arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($Arch -ne "AMD64") {
    throw "unsupported CPU: $Arch (only x86_64 is distributed; use cargo install instead)"
}
$Target = "x86_64-pc-windows-msvc"
$Archive = "mikke-$Variant-$Target.zip"

# Resolve latest to a tag first so that the archive and SHA256SUMS come from the same
# release even if a new one is published between the downloads.
if ($Version -ceq "latest") {
    $Version = (Invoke-WithoutProgress { Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest" }).tag_name
    if ($Version -cnotmatch "^v[0-9]+\.[0-9]+\.[0-9]+$") {
        throw "failed to resolve the latest version (got: $Version)"
    }
}
$Base = "https://github.com/$Repo/releases/download/$Version"

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("mikke-install-" + [System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    Write-Host "download: $Base/$Archive"
    Invoke-WithoutProgress {
        Invoke-WebRequest -Uri "$Base/$Archive" -OutFile (Join-Path $Tmp $Archive) -UseBasicParsing
        Invoke-WebRequest -Uri "$Base/SHA256SUMS" -OutFile (Join-Path $Tmp "SHA256SUMS") -UseBasicParsing
    }

    # Verify the checksum (match the file name field exactly; the binary-mode "*name" form is accepted too).
    $Line = Get-Content (Join-Path $Tmp "SHA256SUMS") | Where-Object {
        $f = ($_ -split "\s+")[1]
        $f -eq $Archive -or $f -eq "*$Archive"
    } | Select-Object -First 1
    if (-not $Line) { throw "SHA256SUMS has no entry for $Archive" }
    $Expected = ($Line -split "\s+")[0].ToLower()
    $Actual = (Get-FileHash (Join-Path $Tmp $Archive) -Algorithm SHA256).Hash.ToLower()
    if ($Expected -ne $Actual) { throw "checksum mismatch" }

    # Extract only the mikke.exe entry to an explicit path instead of expanding the whole
    # zip (guards against path traversal).
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $Exe = Join-Path $Tmp "mikke.exe"
    $Zip = [System.IO.Compression.ZipFile]::OpenRead((Join-Path $Tmp $Archive))
    try {
        $Entry = $Zip.Entries | Where-Object { $_.FullName -eq "mikke.exe" } | Select-Object -First 1
        if (-not $Entry) { throw "unexpected archive contents (mikke.exe not found)" }
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($Entry, $Exe, $true)
    }
    finally { $Zip.Dispose() }
    if (-not (Test-Path $Exe -PathType Leaf)) { throw "failed to extract mikke.exe" }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item $Exe (Join-Path $InstallDir "mikke.exe") -Force
    Write-Host "installed: $(Join-Path $InstallDir 'mikke.exe')"
    & (Join-Path $InstallDir "mikke.exe") --version

    # Check PATH (print a hint if the install dir is in neither the current session nor
    # the user environment variable).
    $Paths = ($env:Path -split ";") + ([Environment]::GetEnvironmentVariable("Path", "User") -split ";")
    $PathContainsInstallDir = $false
    foreach ($PathEntry in $Paths) {
        $NormalizedPathEntry = ConvertTo-NormalizedPath $PathEntry
        if ($NormalizedPathEntry -and [StringComparer]::OrdinalIgnoreCase.Equals($NormalizedPathEntry, $InstallDir)) {
            $PathContainsInstallDir = $true
            break
        }
    }
    if (-not $PathContainsInstallDir) {
        Write-Host "note: $InstallDir is not on PATH. Add it with (effective in a new terminal):"
        Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path','User') + ';' + '$InstallDir', 'User')"
    }
}
finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
