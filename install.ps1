#Requires -Version 5
# mikke の Windows 用インストールスクリプト。Releases から zip を取得し、
# SHA256SUMS で検証してから配置する。
#
#   irm https://raw.githubusercontent.com/kimushun1101/mikke/main/install.ps1 | iex
#
# オプションは環境変数で指定する (iex 経由ではパラメータを渡せないため):
#   $env:MIKKE_VARIANT     = "full"      # slim (既定) / full
#   $env:MIKKE_VERSION     = "v0.2.0"    # 既定: latest
#   $env:MIKKE_INSTALL_DIR = "C:/tools"  # 既定: $env:LOCALAPPDATA/Programs/mikke
#
# Linux / macOS は install.sh を使う。

$ErrorActionPreference = "Stop"

$Repo = "kimushun1101/mikke"
$Variant = if ($env:MIKKE_VARIANT) { $env:MIKKE_VARIANT } else { "slim" }
$Version = if ($env:MIKKE_VERSION) { $env:MIKKE_VERSION } else { "latest" }
$InstallDir = if ($env:MIKKE_INSTALL_DIR) { $env:MIKKE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\mikke" }

if (@("slim", "full") -notcontains $Variant) { throw "MIKKE_VARIANT は slim か full (指定値: $Variant)" }
if ($Version -match "^[0-9]") { $Version = "v$Version" }

# arch 判定 (Windows 向けは x86_64 のみ配布)
if ($env:PROCESSOR_ARCHITECTURE -ne "AMD64") {
    throw "未対応の CPU: $($env:PROCESSOR_ARCHITECTURE) (x86_64 のみ配布。cargo install を使う)"
}
$Target = "x86_64-pc-windows-msvc"
$Archive = "mikke-$Variant-$Target.zip"

# latest はタグ名に解決してから使う (取得の合間に新 release が出ても世代がずれないように)
if ($Version -eq "latest") {
    $Version = (Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest").tag_name
    if ($Version -notmatch "^v[0-9]") { throw "latest バージョンの解決に失敗 (取得値: $Version)" }
}
$Base = "https://github.com/$Repo/releases/download/$Version"

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("mikke-install-" + [System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    Write-Host "download: $Base/$Archive"
    Invoke-WebRequest -Uri "$Base/$Archive" -OutFile (Join-Path $Tmp $Archive) -UseBasicParsing
    Invoke-WebRequest -Uri "$Base/SHA256SUMS" -OutFile (Join-Path $Tmp "SHA256SUMS") -UseBasicParsing

    # checksum 検証 (ファイル名はフィールド完全一致で照合、"*" 付きのバイナリモード形式も許容)
    $Line = Get-Content (Join-Path $Tmp "SHA256SUMS") | Where-Object {
        $f = ($_ -split "\s+")[1]
        $f -eq $Archive -or $f -eq "*$Archive"
    } | Select-Object -First 1
    if (-not $Line) { throw "SHA256SUMS に $Archive の行が無い" }
    $Expected = ($Line -split "\s+")[0].ToLower()
    $Actual = (Get-FileHash (Join-Path $Tmp $Archive) -Algorithm SHA256).Hash.ToLower()
    if ($Expected -ne $Actual) { throw "checksum 不一致" }

    # zip 全展開はせず mikke.exe エントリだけを明示パスへ抽出する (パストラバーサル対策)
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $Exe = Join-Path $Tmp "mikke.exe"
    $Zip = [System.IO.Compression.ZipFile]::OpenRead((Join-Path $Tmp $Archive))
    try {
        $Entry = $Zip.Entries | Where-Object { $_.FullName -eq "mikke.exe" } | Select-Object -First 1
        if (-not $Entry) { throw "アーカイブの内容が想定と異なる (mikke.exe が無い)" }
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($Entry, $Exe, $true)
    }
    finally { $Zip.Dispose() }
    if (-not (Test-Path $Exe -PathType Leaf)) { throw "mikke.exe の抽出に失敗" }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item $Exe (Join-Path $InstallDir "mikke.exe") -Force
    Write-Host "installed: $(Join-Path $InstallDir 'mikke.exe')"
    & (Join-Path $InstallDir "mikke.exe") --version

    # PATH 確認 (現在のセッションとユーザー環境変数のどちらにも無ければ案内)
    $Paths = ($env:Path -split ";") + ([Environment]::GetEnvironmentVariable("Path", "User") -split ";")
    if ($Paths -notcontains $InstallDir) {
        Write-Host "note: $InstallDir に PATH が通っていない。次で追加できる (新しいターミナルから有効):"
        Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path','User') + ';' + '$InstallDir', 'User')"
    }
}
finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
