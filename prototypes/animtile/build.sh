#!/bin/zsh
# 使い方: build.sh <出力先ディレクトリ> <本物のアプリのパス> [起動用アプリの bundle ID]
set -euo pipefail
here=${0:A:h}; out=$1; target=$2; bid=${3:-app.dockmorph.proto.anim}
app="$out/DMAnim.app"; plug="$app/Contents/PlugIns/DMAnimTile.docktileplugin"
rm -rf "$app"; mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$plug/Contents/MacOS" "$plug/Contents/Resources"
swiftc -O -o "$app/Contents/MacOS/DMAnim" "$here/main.swift" "$here/Frames.swift"
swiftc -O -emit-library -module-name DMAnimTile -o "$plug/Contents/MacOS/DMAnimTile" "$here/Plugin.swift" "$here/Frames.swift"
cp "$here/anim.gif" "$app/Contents/Resources/"; cp "$here/anim.gif" "$plug/Contents/Resources/"
cat > "$plug/Contents/Info.plist" <<P
<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>app.dockmorph.proto.anim.tile</string>
<key>CFBundleExecutable</key><string>DMAnimTile</string>
<key>CFBundlePackageType</key><string>BNDL</string>
<key>NSPrincipalClass</key><string>DMAnimTilePlugin</string>
</dict></plist>
P
cat > "$app/Contents/Info.plist" <<P
<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>$bid</string>
<key>CFBundleVersion</key><string>0</string>
<key>CFBundleShortVersionString</key><string>0</string>
<key>CFBundleName</key><string>DMAnim</string>
<key>CFBundleExecutable</key><string>DMAnim</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSDockTilePlugIn</key><string>DMAnimTile.docktileplugin</string>
<key>DMTargetApp</key><string>$target</string>
</dict></plist>
P
codesign --force -s - "$plug"; codesign --force -s - "$app"
echo "$app"
