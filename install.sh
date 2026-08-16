#!/bin/sh
# mikke のインストールスクリプト。Releases から自環境向けバイナリを取得し、
# SHA256SUMS で検証してから配置する。
#
#   curl -fsSL https://raw.githubusercontent.com/kimushun1101/mikke/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/kimushun1101/mikke/main/install.sh | sh -s -- --full
#
# 対象は Linux (x86_64) と macOS (Apple Silicon)。それ以外は cargo install か手動導入 (README 参照)。
set -eu

REPO="kimushun1101/mikke"
VARIANT="${MIKKE_VARIANT:-slim}"
VERSION="${MIKKE_VERSION:-latest}"
INSTALL_DIR="${MIKKE_INSTALL_DIR:-$HOME/.local/bin}"
TARGET="${MIKKE_TARGET:-}"

usage() {
  cat <<'EOF'
usage: install.sh [options]

options:
  --slim            BM25 のみの slim 版を入れる (既定)
  --full            semantic 検索入りの full 版を入れる
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

while [ $# -gt 0 ]; do
  case "$1" in
    --slim) VARIANT=slim ;;
    --full) VARIANT=full ;;
    --version) [ $# -ge 2 ] || err "--version には値が必要 (例: --version v0.2.0)"; VERSION="$2"; shift ;;
    --to) [ $# -ge 2 ] || err "--to には値が必要"; INSTALL_DIR="$2"; shift ;;
    --target) [ $# -ge 2 ] || err "--target には値が必要"; TARGET="$2"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) err "不明なオプション: $1 (--help で一覧)" ;;
  esac
  shift
done

command -v curl >/dev/null 2>&1 || err "curl が必要"
command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 || err "sha256sum か shasum が必要"

case "$VARIANT" in
  slim|full) ;;
  *) err "VARIANT は slim か full (指定値: $VARIANT)" ;;
esac
case "$VERSION" in
  latest|v*) ;;
  [0-9]*) VERSION="v$VERSION" ;;
  *) err "VERSION の形式が不正 (例: v0.2.0)" ;;
esac

# target 自動判定。Linux は glibc バージョン非依存の musl (完全静的リンク) を既定にする
if [ -z "$TARGET" ]; then
  os=$(uname -s)
  arch=$(uname -m)
  case "$os/$arch" in
    Linux/x86_64) TARGET=x86_64-unknown-linux-musl ;;
    Darwin/arm64) TARGET=aarch64-apple-darwin ;;
    *) err "未対応の環境: $os/$arch。cargo install を使う (README 参照)" ;;
  esac
fi

archive="mikke-${VARIANT}-${TARGET}.tar.gz"

# latest はタグ名に解決してから使う (アーカイブと SHA256SUMS の取得の合間に
# 新 release が出ても世代がずれないよう、以降は固定タグの URL で取得する)
if [ "$VERSION" = "latest" ]; then
  VERSION=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest" | sed -n 's|.*/tag/||p')
  [ -n "$VERSION" ] || err "latest バージョンの解決に失敗"
fi
base="https://github.com/${REPO}/releases/download/${VERSION}"

tmp=$(mktemp -d "${TMPDIR:-/tmp}/mikke-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

printf '%s\n' "download: $base/$archive"
curl -fsSL -o "$tmp/$archive" "$base/$archive" \
  || err "ダウンロード失敗。この target/version の組に asset が無い可能性 (Releases を確認)"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || err "SHA256SUMS の取得に失敗"

# checksum 検証 (Linux: sha256sum / macOS: shasum)。ファイル名はフィールド完全一致で照合
checksum_line=$(awk -v f="$archive" '$2 == f' "$tmp/SHA256SUMS")
[ -n "$checksum_line" ] || err "SHA256SUMS に $archive の行が無い"
if command -v sha256sum >/dev/null 2>&1; then
  printf '%s\n' "$checksum_line" | (cd "$tmp" && sha256sum -c -) >/dev/null || err "checksum 不一致"
else
  printf '%s\n' "$checksum_line" | (cd "$tmp" && shasum -a 256 -c -) >/dev/null || err "checksum 不一致"
fi

# mikke メンバーだけを展開し、通常ファイルであることを確認してから配置する
tar xzf "$tmp/$archive" -C "$tmp" mikke
{ [ -f "$tmp/mikke" ] && [ ! -L "$tmp/mikke" ]; } || err "アーカイブの内容が想定と異なる (mikke が通常ファイルでない)"
mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/mikke" "$INSTALL_DIR/mikke"

printf '%s\n' "installed: $INSTALL_DIR/mikke"
"$INSTALL_DIR/mikke" --version

case ":$PATH:" in
  *":$INSTALL_DIR:"*) : ;;
  *) printf '%s\n' "note: $INSTALL_DIR に PATH が通っていない。shell 設定への追加が必要" ;;
esac
