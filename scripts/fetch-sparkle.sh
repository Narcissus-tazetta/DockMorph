#!/bin/zsh
# 配布用ビルドに同梱する Sparkle.framework を取得して、そのパスを標準出力に出す。
# 版とチェックサムは固定(GitHub のリリース資産の digest)。違うファイルなら止める。
set -euo pipefail
version=2.10.1
sha256=1a856d2a9edb8e628de88121d7be04cf0e2167f8641650b78fca00b14244cf27
root=${0:A:h:h}
dir="$root/target/sparkle/$version"
framework="$dir/Sparkle.framework"
if [[ ! -d $framework ]]; then
  mkdir -p "$dir"
  archive=$(mktemp)
  curl -fsSL -o "$archive" "https://github.com/sparkle-project/Sparkle/releases/download/$version/Sparkle-$version.tar.xz" >&2
  actual=$(shasum -a 256 "$archive" | cut -d' ' -f1)
  if [[ $actual != $sha256 ]]; then
    rm -f "$archive"
    echo "Sparkle $version のチェックサムが一致しない: $actual" >&2
    exit 1
  fi
  tar -xJf "$archive" -C "$dir" >&2
  rm -f "$archive"
fi
[[ -d $framework ]] || { echo "Sparkle.framework が見つからない: $framework" >&2; exit 1; }
echo "$framework"
