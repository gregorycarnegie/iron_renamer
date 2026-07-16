#!/bin/sh
# Assembles dist/Iron Renamer.app from the release binary. macOS only (sips, iconutil).
# Usage: macos_app.sh <tag>   e.g. macos_app.sh v0.3.0
set -e
VERSION="${1#v}"
APP="dist/Iron Renamer.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" dist/icon.iconset
cp target/release/iron_renamer "$APP/Contents/MacOS/"
sed "s/__VERSION__/$VERSION/" packaging/Info.plist > "$APP/Contents/Info.plist"

gen() { sips -z "$1" "$1" ui/icon.png --out "dist/icon.iconset/$2.png" >/dev/null; }
gen 16 icon_16x16
gen 32 icon_16x16@2x
gen 32 icon_32x32
gen 64 icon_32x32@2x
gen 128 icon_128x128
gen 256 icon_128x128@2x
gen 256 icon_256x256
iconutil -c icns dist/icon.iconset -o "$APP/Contents/Resources/icon.icns"
