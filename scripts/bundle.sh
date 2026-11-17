#!/bin/zsh
# 使い方: scripts/bundle.sh [debug|release] [出力ディレクトリ]
# 環境変数:
#   DOCKMORPH_VERSION    X.Y.Z を指定すると配布用ビルド。版数を刻み、Sparkle を同梱して更新先と公開鍵を入れる
#   SPARKLE_FRAMEWORK    配布用ビルドで使う Sparkle.framework(既定は scripts/fetch-sparkle.sh で取得)
#   SPARKLE_FEED_URL     配布用ビルドの更新先(既定は main の docs/appcast.xml)
set -euo pipefail
profile=${1:-debug}
out=${2:-target/bundle}
root=${0:A:h:h}
cd "$root"
if [[ $profile == release ]]; then cargo build --release -p dockmorph-app; else cargo build -p dockmorph-app; fi
version=${DOCKMORPH_VERSION:-0.1.0}
build_number=1
if [[ -n ${DOCKMORPH_VERSION:-} ]]; then
  build_number=$(bash scripts/derive_build_number.sh "$DOCKMORPH_VERSION")
  sparkle=${SPARKLE_FRAMEWORK:-$(scripts/fetch-sparkle.sh)}
  [[ -d $sparkle ]] || { echo "Sparkle.framework が見つからない: $sparkle" >&2; exit 1; }
fi
app="$out/DockMorph.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "target/$profile/dockmorph-app" "$app/Contents/MacOS/DockMorph"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>app.dockmorph.DockMorph</string>
  <key>CFBundleName</key><string>DockMorph</string>
  <key>CFBundleExecutable</key><string>DockMorph</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$build_number</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleIconFile</key><string>AppIcon</string>
</dict></plist>
PLIST
mkdir -p "$app/Contents/Resources"
swiftc -O -parse-as-library -o "$app/Contents/Resources/DMIconGuard" crates/app/helpers/IconGuard.swift
iconset=$(mktemp -d)/AppIcon.iconset
swift scripts/icon.swift crates/app/assets/menubar.svg "$iconset"
iconutil -c icns -o "$app/Contents/Resources/AppIcon.icns" "$iconset"
rm -rf "${iconset:h}"
# Dock の GIF 表示(実験的)で使う起動用アプリの雛形。Info.plist と画像は DockMorph が登録ごとに書く
tpl="$app/Contents/Resources/Launcher.app"
plug="$tpl/Contents/PlugIns/DMAnimTile.docktileplugin"
mkdir -p "$tpl/Contents/MacOS" "$tpl/Contents/Resources" "$plug/Contents/MacOS"
cp "$app/Contents/Resources/DMIconGuard" "$tpl/Contents/Resources/DMIconGuard"
swiftc -O -o "$tpl/Contents/MacOS/DMLauncher" crates/app/launcher/main.swift
swiftc -O -emit-library -module-name DMAnimTile -o "$plug/Contents/MacOS/DMAnimTile" crates/app/launcher/Plugin.swift
cat > "$plug/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>app.dockmorph.DockMorph.tileplugin</string>
  <key>CFBundleExecutable</key><string>DMAnimTile</string>
  <key>CFBundlePackageType</key><string>BNDL</string>
  <key>NSPrincipalClass</key><string>DMAnimTilePlugin</string>
</dict></plist>
PLIST
codesign --force -s - "$plug"
# 作成済みの起動用アプリを入れ替えるかどうかを、DockMorph がこの印で判断する
cat "$tpl/Contents/MacOS/DMLauncher" "$plug/Contents/MacOS/DMAnimTile" "$tpl/Contents/Resources/DMIconGuard" | shasum -a 256 | cut -d' ' -f1 > "$tpl/Contents/Resources/dm-template"
# ad-hoc 署名はビルドのたびに別のアプリ扱いになり、アクセシビリティの許可が外れる。証明書で署名すれば残る
identity=${DOCKMORPH_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk '/Apple Development/ { print $2; exit }')}
if [[ -n ${DOCKMORPH_VERSION:-} ]]; then
  feed=${SPARKLE_FEED_URL:-https://raw.githubusercontent.com/Narcissus-tazetta/DockMorph/main/docs/appcast.xml}
  mkdir -p "$app/Contents/Frameworks"
  cp -R "$sparkle" "$app/Contents/Frameworks/"
  /usr/libexec/PlistBuddy -c "Add :SUFeedURL string $feed" "$app/Contents/Info.plist"
  /usr/libexec/PlistBuddy -c "Add :SUPublicEDKey string $(tr -d '[:space:]' < scripts/sparkle-public-key.txt)" "$app/Contents/Info.plist"
  # Sparkle は XPC と Updater.app を中に持つので、フレームワークだけ --deep で入れ子ごと署名する(アプリ本体には使わない)
  codesign --force --deep -s "${identity:--}" "$app/Contents/Frameworks/Sparkle.framework"
fi
codesign --force -s "${identity:--}" "$app"
codesign --verify --deep --strict "$app"
echo "$app"
