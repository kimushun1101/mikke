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
# starts ("active: ...") and warns when that is not the one just installed. The other
# copy is only named, never run, overwritten or reordered.
#
# The whole body runs in a script block so that `irm | iex` leaves no functions or
# variables behind in the caller's session.
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

& {
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

# Broadcast WM_SETTINGCHANGE("Environment") so that Explorer and new terminals reload the
# user PATH from the registry. Returns $false when the broadcast did not go through
# (then a re-login is needed); never throws.
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
        $Ret = [MikkeInstaller.NativeMethods]::SendMessageTimeout($HWND_BROADCAST, $WM_SETTINGCHANGE, [UIntPtr]::Zero, "Environment", $SMTO_ABORTIFHUNG, 5000, [ref]$Result)
        return ($Ret -ne [IntPtr]::Zero)
    }
    catch {
        return $false
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
                if (Send-EnvironmentChange) {
                    Write-Host "note: added $InstallDir to the user PATH (new terminals pick it up)"
                }
                else {
                    Write-Host "note: added $InstallDir to the user PATH, but running programs could not be notified; terminals opened before a re-login may not see it"
                }
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
    # the same dir. So the comparison is per file, not per directory. The other copy is only
    # named, never executed: it is an arbitrary program on PATH that could block or have
    # side effects.
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
            Write-Warning "another mikke is found first on PATH and shadows ${InstalledExe}: $ActivePath"
            Write-Warning "update or remove it with whatever installed it (cargo: 'cargo uninstall mikke'), put $InstallDir earlier on PATH, or delete it if it sits in ${InstallDir}; nothing is overwritten or reordered automatically"
        }
    }
    else {
        Write-Host "active: $($Resolved.CommandType) mikke"
        Write-Warning "mikke is a $($Resolved.CommandType) in this session and takes precedence over $InstalledExe; remove that definition (e.g. Remove-Item Function:mikke or Alias:mikke) or run the exe by full path"
    }
}
finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
}
