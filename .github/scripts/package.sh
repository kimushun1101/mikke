#!/usr/bin/env bash
# リリースアセットの梱包: バイナリを mikke-<flavor>-<target>.tar.gz (Windows は .zip) に固める
set -euo pipefail

flavor="$1" # slim | full
target="$2"

ext=""
case "$target" in *windows*) ext=".exe" ;; esac

mkdir -p dist stage
cp "target/$target/release/mikke$ext" "stage/mikke$ext"

if [ -n "$ext" ]; then
  (cd stage && 7z a "../dist/mikke-$flavor-$target.zip" "mikke$ext")
else
  tar czf "dist/mikke-$flavor-$target.tar.gz" -C stage "mikke$ext"
fi
rm -rf stage
