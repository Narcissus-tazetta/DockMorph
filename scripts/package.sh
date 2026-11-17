#!/bin/zsh
# 配布物(dist/DockMorph-macos-vX.Y.Z.zip と .dmg)を作る。zip は Sparkle と Homebrew 用、dmg は手動ダウンロード用。
#   scripts/package.sh X.Y.Z
set -euo pipefail
root=${0:A:h:h}
cd "$root"
version=${1:?version required (X.Y.Z)}
dist=dist
zip="$dist/DockMorph-macos-v$version.zip"
dmg="$dist/DockMorph-macos-v$version.dmg"

rm -rf "$dist"
mkdir -p "$dist"
DOCKMORPH_VERSION=$version scripts/bundle.sh release "$dist" > /dev/null
app="$dist/DockMorph.app"

archs=$(lipo -archs "$app/Contents/MacOS/DockMorph")
[[ $archs == arm64 ]] || { echo "DockMorph のアーキテクチャが '$archs'(期待: arm64)" >&2; exit 1; }

# Sparkle が展開して Info.plist を読むので、親フォルダごと固めた zip にする
ditto -c -k --sequesterRsrc --keepParent "$app" "$zip"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -volname DockMorph -srcfolder "$stage" -ov -format UDZO "$dmg" > /dev/null

ls -lh "$zip" "$dmg"
