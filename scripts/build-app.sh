#!/usr/bin/env bash
# Assemble Clowder.app: the SwiftPM app exe + the three release Rust binaries + Info.plist + icon.
# Usage: scripts/build-app.sh [output-dir]   (default: dist/)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT_DIR="${1:-$ROOT/dist}"
APP="$OUT_DIR/Clowder.app"
VERSION="$(tr -d '[:space:]' < "$ROOT/VERSION")"

echo "==> Building Rust binaries (release)"
( cd "$ROOT" && cargo build --release -p clowder-daemon -p clowder-client -p clowder-hook )

echo "==> Building macOS app (release)"
( cd "$ROOT/macos" && swift build -c release )
APP_EXE="$ROOT/macos/.build/release/clowder-app"
[ -x "$APP_EXE" ] || { echo "missing app exe: $APP_EXE" >&2; exit 1; }

echo "==> Assembling $APP (version $VERSION)"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/bin"

cp "$APP_EXE" "$APP/Contents/MacOS/clowder-app"
for bin in clowder-daemon clowder clowder-hook; do
  cp "$ROOT/target/release/$bin" "$APP/Contents/Resources/bin/$bin"
done

echo "==> Generating placeholder icon"
ICONSET="$(mktemp -d)/Clowder.iconset"; mkdir -p "$ICONSET"
BASE_PNG="$(mktemp -d)/icon.png"
swift "$ROOT/scripts/gen-icon.swift" "$BASE_PNG" 1024
for sz in 16 32 128 256 512; do
  sips -z "$sz" "$sz"       "$BASE_PNG" --out "$ICONSET/icon_${sz}x${sz}.png" >/dev/null
  sips -z $((sz*2)) $((sz*2)) "$BASE_PNG" --out "$ICONSET/icon_${sz}x${sz}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Clowder.icns"

echo "==> Writing Info.plist"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>Clowder</string>
    <key>CFBundleDisplayName</key>     <string>Clowder</string>
    <key>CFBundleIdentifier</key>      <string>com.github.richardcase.clowder</string>
    <key>CFBundleExecutable</key>      <string>clowder-app</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleShortVersionString</key> <string>$VERSION</string>
    <key>CFBundleVersion</key>         <string>$VERSION</string>
    <key>CFBundleIconFile</key>        <string>Clowder</string>
    <key>LSMinimumSystemVersion</key>  <string>14.0</string>
    <key>NSHighResolutionCapable</key> <true/>
</dict>
</plist>
PLIST

echo "==> Done: $APP"
