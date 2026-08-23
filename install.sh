#!/bin/sh
# mikke のインストールスクリプト。Releases から自環境向けバイナリを取得し、
# SHA256SUMS で検証してから配置する。
#
#   curl -fsSL https://raw.githubusercontent.com/kimushun1101/mikke/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/kimushun1101/mikke/main/install.sh | sh -s -- --slim
#
# 対象は Linux (x86_64) と macOS (Apple Silicon)。Windows は install.ps1、
# それ以外は cargo install か手動導入 (README 参照)。
set -eu

REPO="kimushun1101/mikke"
VARIANT="${MIKKE_VARIANT:-full}"
VERSION="${MIKKE_VERSION:-latest}"
# 既定の $HOME/.local/bin は引数パースの後に補う (HOME 未設定でも --to を効かせるため)。
# 明示指定されたかは別に持つ ("+set" は空文字での指定も set と数える) — 空文字を
# 黙って既定へ落とすと、意図しない場所へ入ってしまうため error にする。
INSTALL_DIR="${MIKKE_INSTALL_DIR:-}"
INSTALL_DIR_SET="${MIKKE_INSTALL_DIR+yes}"
TARGET="${MIKKE_TARGET:-}"

usage() {
  cat <<'EOF'
usage: install.sh [options]

options:
  --full            semantic 検索入りの full 版を入れる (既定)
  --slim            BM25 のみの slim 版を入れる (最小サイズ)
  --version vX.Y.Z  インストールするバージョン (既定: latest)
  --to DIR          配置先ディレクトリ (既定: ~/.local/bin)
  --target TRIPLE   target triple の手動指定 (例: x86_64-unknown-linux-gnu)
  -h, --help        このヘルプ

環境変数 MIKKE_VARIANT / MIKKE_VERSION / MIKKE_INSTALL_DIR / MIKKE_TARGET でも指定できる。
EOF
}

err() {
  printf 'error: %s\n' "$1" >&2
  exit 1
}

# curl の失敗を exit code で出し分ける (22 = HTTP エラー応答 / 5,6,7,28,35 = 接続系)
curl_err() {
  case "$2" in
    22) err "$1に失敗 (HTTP エラー応答)。$3" ;;
    5|6|7|28|35) err "$1に失敗 (ネットワークに到達できない: curl exit $2)。接続と proxy 設定を確認" ;;
    *) err "$1に失敗 (curl exit $2)" ;;
  esac
}

while [ $# -gt 0 ]; do
  case "$1" in
    --slim) VARIANT=slim ;;
    --full) VARIANT=full ;;
    --version) [ $# -ge 2 ] || err "--version には値が必要 (例: --version v0.2.0)"; VERSION="$2"; shift ;;
    --to) [ $# -ge 2 ] || err "--to には値が必要"; INSTALL_DIR="$2"; INSTALL_DIR_SET=yes; shift ;;
    --target) [ $# -ge 2 ] || err "--target には値が必要"; TARGET="$2"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) err "不明なオプション: $1 (--help で一覧)" ;;
  esac
  shift
done

if [ -z "$INSTALL_DIR" ]; then
  [ -z "$INSTALL_DIR_SET" ] || err "配置先が空文字。--to / MIKKE_INSTALL_DIR にはディレクトリを指定する"
  [ -n "${HOME:-}" ] || err "配置先が決められない (HOME が未設定)。--to DIR か MIKKE_INSTALL_DIR で指定する"
  INSTALL_DIR="$HOME/.local/bin"
fi

# 同名ディレクトリがあると install は中へ置いてしまうので、ダウンロード前に弾く
# (配置直前の再検査は不要 — install 後の通常ファイル確認が最終的な砦になる)
[ ! -d "$INSTALL_DIR/mikke" ] || err "配置先に mikke という名のディレクトリがある: $INSTALL_DIR/mikke (退かすか --to で別の場所を指定)"

command -v curl >/dev/null 2>&1 || err "curl が必要"
command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 || err "sha256sum か shasum が必要"

case "$VARIANT" in
  slim|full) ;;
  *) err "VARIANT は slim か full (指定値: $VARIANT)" ;;
esac
# latest / X.Y.Z / vX.Y.Z の完全一致のみ許可する (前方一致の case glob だと
# 例えば "v0.3.0/../../x" のような文字列も v[0-9]* に一致してしまい、そのまま
# release の URL 組み立てに使われるため grep -E で厳密に検査する)
if printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  VERSION="v$VERSION"
fi
if [ "$VERSION" != latest ] && ! printf '%s' "$VERSION" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
  err "VERSION は latest か X.Y.Z か vX.Y.Z (指定値: $VERSION)"
fi

# target 自動判定。Linux は glibc バージョン非依存の musl (完全静的リンク) を既定にする
if [ -z "$TARGET" ]; then
  os=$(uname -s)
  arch=$(uname -m)
  case "$os/$arch" in
    Linux/x86_64) TARGET=x86_64-unknown-linux-musl ;;
    Darwin/arm64) TARGET=aarch64-apple-darwin ;;
    *) err "未対応の環境: $os/$arch。Windows は install.ps1、その他は cargo install (README 参照)" ;;
  esac
fi

archive="mikke-${VARIANT}-${TARGET}.tar.gz"

# latest はタグ名に解決してから使う (アーカイブと SHA256SUMS の取得の合間に
# 新 release が出ても世代がずれないよう、以降は固定タグの URL で取得する)
if [ "$VERSION" = "latest" ]; then
  rc=0
  latest_url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest") || rc=$?
  [ "$rc" -eq 0 ] || curl_err "latest バージョンの解決" "$rc" "Releases が公開されているか確認"
  VERSION=$(printf '%s\n' "$latest_url" | sed -n 's|.*/tag/||p')
  case "$VERSION" in
    v[0-9]*) ;;
    *) err "latest バージョンの解決に失敗 (取得値: $VERSION)" ;;
  esac
fi
base="https://github.com/${REPO}/releases/download/${VERSION}"

tmp=$(mktemp -d "${TMPDIR:-/tmp}/mikke-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

printf '%s\n' "download: $base/$archive"
rc=0
curl -fsSL -o "$tmp/$archive" "$base/$archive" || rc=$?
[ "$rc" -eq 0 ] || curl_err "ダウンロード" "$rc" "この target/version の組に asset が無い可能性 (Releases を確認)"
rc=0
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || rc=$?
[ "$rc" -eq 0 ] || curl_err "SHA256SUMS の取得" "$rc" "$VERSION の release に SHA256SUMS が無い可能性"

# checksum 検証 (Linux: sha256sum / macOS: shasum)。ファイル名はフィールド完全一致で照合
# ("*" 付きのバイナリモード形式にも耐える)
checksum_line=$(awk -v f="$archive" '$2 == f || $2 == ("*" f)' "$tmp/SHA256SUMS")
[ -n "$checksum_line" ] || err "SHA256SUMS に $archive の行が無い"
if command -v sha256sum >/dev/null 2>&1; then
  printf '%s\n' "$checksum_line" | (cd "$tmp" && sha256sum -c -) >/dev/null || err "checksum 不一致"
else
  printf '%s\n' "$checksum_line" | (cd "$tmp" && shasum -a 256 -c -) >/dev/null || err "checksum 不一致"
fi
printf '%s\n' "verified: SHA256SUMS OK"

# mikke メンバーだけを展開し、通常ファイルであることを確認してから配置する
tar xzf "$tmp/$archive" -C "$tmp" mikke || err "アーカイブの展開に失敗 ($archive)"
{ [ -f "$tmp/mikke" ] && [ ! -L "$tmp/mikke" ]; } || err "アーカイブの内容が想定と異なる (mikke が通常ファイルでない)"
mkdir -p "$INSTALL_DIR" || err "配置先ディレクトリを作れない: $INSTALL_DIR"
install -m 755 "$tmp/mikke" "$INSTALL_DIR/mikke" || err "配置に失敗: $INSTALL_DIR/mikke"
[ -f "$INSTALL_DIR/mikke" ] || err "配置後に $INSTALL_DIR/mikke が通常ファイルとして見つからない"

# 起動確認まで通ってから成功を名乗る (target 取り違えで動かないバイナリを残さない)
rc=0
smoke=$("$INSTALL_DIR/mikke" --version 2>&1) || rc=$?
if [ "$rc" -ne 0 ]; then
  printf '%s\n' "$smoke" >&2
  rm -f "$INSTALL_DIR/mikke" 2>/dev/null || :
  if [ -e "$INSTALL_DIR/mikke" ]; then
    leftover="削除できなかったので $INSTALL_DIR/mikke が残っている"
  else
    leftover="配置したファイルは削除した"
  fi
  err "配置したバイナリがこの環境で起動しない (target: $TARGET, exit $rc)。$leftover。Linux では glibc 非依存の --target x86_64-unknown-linux-musl (既定) を使う"
fi

printf '%s\n' "version: $smoke"
printf '%s\n' "installed: $INSTALL_DIR/mikke"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) : ;;
  *) printf '%s\n' "note: $INSTALL_DIR に PATH が通っていない。shell 設定への追加が必要" ;;
esac
