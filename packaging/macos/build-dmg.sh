#!/bin/bash
# Builds "Talking Mouse.app" (universal arm64 + x86_64) and wraps it in a DMG.
# Expects release builds for both targets:
#   cargo build --release --target aarch64-apple-darwin
#   cargo build --release --target x86_64-apple-darwin
# Usage: packaging/macos/build-dmg.sh <version> [output-dir]
set -euo pipefail

version="$1"
out="${2:-dist}"
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
app="$work/dmg/Talking Mouse.app"

mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$out"
for bin in talking-mouse talking-mouse-editor; do
    lipo -create \
        "$root/target/aarch64-apple-darwin/release/$bin" \
        "$root/target/x86_64-apple-darwin/release/$bin" \
        -output "$app/Contents/MacOS/$bin"
done
sed "s/@VERSION@/$version/g" "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
cp "$root/assets/icon/icon.icns" "$app/Contents/Resources/AppIcon.icns"

# Optional bundled ffmpeg: drop ffmpeg + ffprobe into packaging/macos/ffmpeg/.
if [ -x "$root/packaging/macos/ffmpeg/ffmpeg" ]; then
    cp -R "$root/packaging/macos/ffmpeg" "$app/Contents/Resources/ffmpeg"
fi

# Ad-hoc signature so Apple Silicon will run it; not notarised.
codesign --force --deep --sign - "$app"

ln -s /Applications "$work/dmg/Applications"
dmg="$out/TalkingMouse-$version-macos-universal.dmg"
hdiutil create -volname "Talking Mouse" -srcfolder "$work/dmg" -ov -format UDZO "$dmg"
rm -rf "$work"
echo "Built $dmg"
