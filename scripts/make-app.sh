#!/usr/bin/env bash
# Build "All In One.app" and a drag-to-Applications .dmg (macOS only).
# Usage: scripts/make-app.sh [vX.Y.Z]   -> dist/All-In-One-<arch>.dmg
# Uses target/release/all-in-one if present, otherwise builds it.
set -euo pipefail
cd "$(dirname "$0")/.."
[ "$(uname)" = Darwin ] || { echo "macOS only" >&2; exit 1; }

version=0.0.0
if [ -n "${1:-}" ]; then version="${1#v}"; else
  version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
fi
arch=$(uname -m); case "$arch" in arm64) arch=aarch64;; esac

[ -x target/release/all-in-one ] || cargo build --release -p aio-app

app="dist/All In One.app"
rm -rf "$app" dist/dmg-stage; mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" dist
cp target/release/all-in-one "$app/Contents/MacOS/all-in-one"

# Icon: assets/icon-1024.png -> AppIcon.icns
iconset=$(mktemp -d)/AppIcon.iconset; mkdir -p "$iconset"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/icon-1024.png --out "$iconset/icon_${s}x${s}.png" >/dev/null
  sips -z $((s*2)) $((s*2)) assets/icon-1024.png --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>All In One</string>
  <key>CFBundleDisplayName</key><string>All In One</string>
  <key>CFBundleIdentifier</key><string>com.lollynixt.allinone</string>
  <key>CFBundleExecutable</key><string>all-in-one</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
</dict></plist>
PLIST

# Ad-hoc sign (required to run on Apple Silicon; not a Developer ID signature).
codesign --force --deep -s - "$app" 2>/dev/null || true

# DMG with an Applications shortcut for drag-and-drop install.
stage=dist/dmg-stage; mkdir -p "$stage"; cp -R "$app" "$stage/"; ln -s /Applications "$stage/Applications"
dmg="dist/All-In-One-$arch.dmg"; rm -f "$dmg"
hdiutil create -volname "All In One" -srcfolder "$stage" -ov -format UDZO "$dmg" >/dev/null
rm -rf "$stage"
echo "Built $dmg"
