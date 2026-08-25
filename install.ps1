# Windows installer for mikke. Downloads the release zip, verifies it against
# SHA256SUMS, and places mikke.exe.
#
#   irm https://raw.githubusercontent.com/kimushun1101/mikke/main/install.ps1 | iex
#
# Options are given via environment variables (parameters cannot be passed through iex):
#   $env:MIKKE_VARIANT        = "slim"      # full (default) / slim
#   $env:MIKKE_VERSION        = "v0.2.0"    # default: latest
#   $env:MIKKE_INSTALL_DIR    = "C:/tools"  # default: $env:USERPROFILE/.local/bin (same as install.sh)
#   $env:MIKKE_NO_MODIFY_PATH = "1"         # do not touch the user PATH; only print how to add it
# MIKKE_TARGET is not provided because there is only one Windows target.
#
# PATH: unlike install.sh, this script adds the install dir to the user PATH (HKCU)
# when it is missing. Windows has no conventional per-user bin directory, so the
# default location is almost never on PATH at first install. Set
# MIKKE_NO_MODIFY_PATH to opt out.
# After placing the exe, the script prints which mikke the current session actually
# starts ("active: ...") and warns when that is not the one just installed: a copy
# earlier on PATH, a mikke.com / mikke.ps1 that wins by PATHEXT order, or an alias /
# function named mikke. Nothing is overwritten or reordered automatically; the warning
# says how to resolve it depending on what owns the other copy (cargo, the old default
# location, something else).
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
$InstallDir = $env:MIKKE_INSTALL_DIR
if (-not $InstallDir) {
    # Same default as install.sh (~/.local/bin). HOME is a fallback for environments that
    # set it without USERPROFILE (MSYS, Cygwin).
    $HomeDir = if ($env:USERPROFILE) { $env:USERPROFILE } elseif ($env:HOME) { $env:HOME } else { $null }
    if (-not $HomeDir) { throw "cannot determine the home directory (USERPROFILE and HOME are both empty); set MIKKE_INSTALL_DIR" }
    $InstallDir = Join-Path $HomeDir ".local\bin"
}

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

# True if a ";"-separated PATH list contains $Dir (compared after normalization).
function Test-PathListContains {
    param([string]$PathList, [string]$Dir)

    foreach ($PathEntry in ($PathList -split ";")) {
        $NormalizedPathEntry = ConvertTo-NormalizedPath $PathEntry
        if ($NormalizedPathEntry -and [StringComparer]::OrdinalIgnoreCase.Equals($NormalizedPathEntry, $Dir)) {
            return $true
        }
    }
    return $false
}

# First line of `<exe> --version`, or a placeholder if it cannot be run. Native stderr under
# $ErrorActionPreference = Stop would otherwise abort the installer for a broken executable.
# The output is collected as a whole: stopping the pipeline early (Select-Object -First)
# can leave the native process with exit code -1 in Windows PowerShell 5.1.
function Get-VersionLine {
    param([string]$Exe)

    $ErrorActionPreference = "Continue"
    try {
        $Line = @(& $Exe --version 2>&1 | ForEach-Object { "$_" })
        if ($Line.Count -gt 0 -and $Line[0]) { return $Line[0] }
        return "no --version output"
    }
    catch {
        return "--version failed"
    }
}

# Broadcast WM_SETTINGCHANGE("Environment") so that Explorer and new terminals reload the
# user PATH from the registry. Best effort: a failure only means a re-login is needed.
function Send-EnvironmentChange {
    try {
        if (-not ("MikkeInstaller.NativeMethods" -as [type])) {
            Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
namespace MikkeInstaller {
    public static class NativeMethods {
        [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
    }
}
"@
        }
        $HWND_BROADCAST = [IntPtr]0xffff
        $WM_SETTINGCHANGE = 0x1A
        $SMTO_ABORTIFHUNG = 0x2
        $Result = [UIntPtr]::Zero
        [MikkeInstaller.NativeMethods]::SendMessageTimeout($HWND_BROADCAST, $WM_SETTINGCHANGE, [UIntPtr]::Zero, "Environment", $SMTO_ABORTIFHUNG, 5000, [ref]$Result) | Out-Null
    }
    catch {
        Write-Host "note: could not notify running programs of the PATH change ($($_.Exception.Message)); terminals started after a re-login will see it"
    }
}

# One line of advice for a mikke that shadows the one just installed, chosen by what owns
# the shadow. Overwriting it here would break its owner (cargo keeps metadata for
# $CARGO_HOME/bin), so the shadow is never touched and PATH is never reordered.
function Get-ShadowAdvice {
    param([string]$ShadowPath, [string]$InstallDir)

    $Compare = [StringComparer]::OrdinalIgnoreCase
    $ShadowDir = ConvertTo-NormalizedPath (Split-Path -Parent $ShadowPath)
    if ($ShadowDir -and $Compare.Equals($ShadowDir, $InstallDir)) {
        return "it is in the install dir and wins over mikke.exe in PowerShell command lookup (.ps1 first, then PATHEXT order); delete $ShadowPath if it is not needed"
    }
    $CargoBinDirs = @()
    if ($env:CARGO_HOME) { $CargoBinDirs += ConvertTo-NormalizedPath (Join-Path $env:CARGO_HOME "bin") }
    if ($env:USERPROFILE) { $CargoBinDirs += ConvertTo-NormalizedPath (Join-Path $env:USERPROFILE ".cargo\bin") }
    foreach ($CargoBinDir in $CargoBinDirs) {
        if ($CargoBinDir -and $ShadowDir -and $Compare.Equals($CargoBinDir, $ShadowDir)) {
            return "it is managed by cargo: keep it and update it by re-running 'cargo install --git https://github.com/kimushun1101/mikke --locked' (mikke is not on crates.io), or run 'cargo uninstall mikke' first to switch to this standalone install"
        }
    }
    if ($env:LOCALAPPDATA) {
        $OldDefaultDir = ConvertTo-NormalizedPath (Join-Path $env:LOCALAPPDATA "Programs\mikke")
        if ($OldDefaultDir -and $ShadowDir -and $Compare.Equals($OldDefaultDir, $ShadowDir)) {
            return "it is a leftover of the old default location: delete $OldDefaultDir and remove that entry from the user PATH"
        }
    }
    return "update or remove it with whatever installed it (package manager, cargo, ...), or put $InstallDir before $ShadowDir on PATH"
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
    $InstalledExe = Join-Path $InstallDir "mikke.exe"
    Copy-Item $Exe $InstalledExe -Force
    Write-Host "installed: $InstalledExe"
    & $InstalledExe --version

    # PATH. The user PATH (HKCU\Environment) is what new terminals inherit. The registry is
    # read and written directly rather than via [Environment]::SetEnvironmentVariable, which
    # would rewrite the value as REG_SZ with every %VAR% reference expanded (the Windows
    # default user PATH is a REG_EXPAND_SZ containing %USERPROFILE%). The containment check
    # uses the expanded value; the write appends to the raw value and keeps REG_EXPAND_SZ.
    # WM_SETTINGCHANGE is broadcast so that new terminals pick the change up without a
    # re-login. The current session is updated too so that `mikke` works right away, but
    # only when the user PATH has the dir (already, or just added): a session-only PATH
    # would make `mikke` work here and nowhere else.
    $ModifyPath = -not $env:MIKKE_NO_MODIFY_PATH
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $OnUserPath = Test-PathListContains $UserPath $InstallDir
    $AddedToUserPath = $false
    if (-not $OnUserPath) {
        if ($ModifyPath) {
            # The install itself succeeded; any registry failure (key missing, access denied,
            # policy) only downgrades to the manual instructions.
            $EnvKey = $null
            try {
                $EnvKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment", $true)
                if (-not $EnvKey) { throw "HKCU\Environment could not be opened for writing" }
                $RawUserPath = [string]$EnvKey.GetValue("Path", "", [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
                $NewUserPath = if ([string]::IsNullOrWhiteSpace($RawUserPath)) { $InstallDir } else { $RawUserPath.TrimEnd(";") + ";" + $InstallDir }
                $EnvKey.SetValue("Path", $NewUserPath, [Microsoft.Win32.RegistryValueKind]::ExpandString)
                $AddedToUserPath = $true
            }
            catch {
                Write-Host "note: could not add $InstallDir to the user PATH ($($_.Exception.Message)). Add it with (effective in a new terminal):"
                Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path','User') + ';' + '$InstallDir', 'User')"
            }
            finally {
                if ($EnvKey) { $EnvKey.Close() }
            }
            if ($AddedToUserPath) {
                Send-EnvironmentChange
                Write-Host "note: added $InstallDir to the user PATH (new terminals pick it up)"
            }
        }
        else {
            Write-Host "note: $InstallDir is not on the user PATH (MIKKE_NO_MODIFY_PATH is set, so it was not added). Re-run without MIKKE_NO_MODIFY_PATH, or add it with (effective in a new terminal):"
            Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path','User') + ';' + '$InstallDir', 'User')"
        }
    }
    if ($ModifyPath -and ($OnUserPath -or $AddedToUserPath) -and -not (Test-PathListContains $env:Path $InstallDir)) {
        $env:Path = if ([string]::IsNullOrWhiteSpace($env:Path)) { $InstallDir } else { $env:Path.TrimEnd(";") + ";" + $InstallDir }
    }

    # Which mikke does this session actually start? Get-Command without a type filter returns
    # the command that would run: an alias or function beats every file, and among files the
    # first PATH dir wins, with PATHEXT deciding between mikke.com / mikke.exe / mikke.ps1 in
    # the same dir. So the comparison is per file, not per directory.
    $ShadowDir = $null
    $Resolved = Get-Command mikke -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $Resolved) {
        if (Test-PathListContains $env:Path $InstallDir) {
            Write-Host "note: mikke does not resolve in this session although $InstallDir is on PATH; run $InstalledExe by full path, or open a new terminal"
        }
    }
    elseif (@("Application", "ExternalScript") -contains "$($Resolved.CommandType)") {
        $ActivePath = ConvertTo-NormalizedPath $Resolved.Source
        if (-not $ActivePath) { $ActivePath = $Resolved.Source }
        Write-Host "active: $ActivePath"
        if (-not [StringComparer]::OrdinalIgnoreCase.Equals($ActivePath, (ConvertTo-NormalizedPath $InstalledExe))) {
            $ShadowDir = ConvertTo-NormalizedPath (Split-Path -Parent $ActivePath)
            $ResolvedVersion = Get-VersionLine $ActivePath
            # The shadow's exit code must not become this script's final native exit code.
            $global:LASTEXITCODE = 0
            Write-Warning "another mikke is found first on PATH: $ActivePath ($ResolvedVersion)"
            Write-Warning (Get-ShadowAdvice $ActivePath $InstallDir)
        }
    }
    else {
        Write-Host "active: $($Resolved.CommandType) mikke"
        Write-Warning "mikke is a $($Resolved.CommandType) in this session and takes precedence over $InstalledExe; remove that definition (e.g. Remove-Item Function:mikke or Alias:mikke) or run the exe by full path"
    }

    # A copy at the pre-#46 default location is a leftover unless that is where we just installed.
    # Skipped when that copy is the one shadowing on PATH: the warning above already covers it.
    if ($env:LOCALAPPDATA) {
        $OldDefaultDir = ConvertTo-NormalizedPath (Join-Path $env:LOCALAPPDATA "Programs\mikke")
        $OldDefaultIsShadow = $ShadowDir -and $OldDefaultDir -and [StringComparer]::OrdinalIgnoreCase.Equals($OldDefaultDir, $ShadowDir)
        if ($OldDefaultDir -and -not $OldDefaultIsShadow -and -not [StringComparer]::OrdinalIgnoreCase.Equals($OldDefaultDir, $InstallDir)) {
            $OldDefaultExe = Join-Path $OldDefaultDir "mikke.exe"
            if (Test-Path $OldDefaultExe -PathType Leaf) {
                Write-Host "note: $OldDefaultExe is a leftover of the old default location; it is unused unless $OldDefaultDir is on PATH and can be deleted"
            }
        }
    }
}
finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
