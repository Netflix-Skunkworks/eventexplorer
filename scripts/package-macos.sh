#!/usr/bin/env bash
# Assemble a Finder-launchable app from an existing release build.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

if [ "$(uname -s)" != Darwin ]; then
    echo "Error: macOS app packaging requires macOS (sips, iconutil, codesign)." >&2
    exit 1
fi
if [ "$#" -gt 1 ]; then
    echo "Usage: $0 [output.app]" >&2
    exit 1
fi

APP="${1:-target/release/EventExplorer.app}"
VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
if [ ! -x target/release/event_explorer ]; then
    echo "Error: run cargo build --release before packaging." >&2
    exit 1
fi
if [ -e "$APP" ]; then
    echo "Error: output already exists: $APP" >&2
    exit 1
fi

ICON_WORK=$(mktemp -d "${TMPDIR:-/tmp}/eventexplorer-icon.XXXXXX")
trap 'rm -rf "$ICON_WORK"' EXIT
mkdir -p "$ICON_WORK/EventExplorer.iconset" "$APP/Contents/MacOS" "$APP/Contents/Resources"

# Produce the standard Retina iconset from the existing project icon.
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" assets/icon_256.png \
        --out "$ICON_WORK/EventExplorer.iconset/icon_${size}x${size}.png" >/dev/null
    retina=$((size * 2))
    sips -z "$retina" "$retina" assets/icon_256.png \
        --out "$ICON_WORK/EventExplorer.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICON_WORK/EventExplorer.iconset" -o "$APP/Contents/Resources/EventExplorer.icns"

cp target/release/event_explorer "$APP/Contents/MacOS/event_explorer"
strip "$APP/Contents/MacOS/event_explorer"
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>event_explorer</string>
    <key>CFBundleIdentifier</key><string>net.netflix.eventexplorer</string>
    <key>CFBundleName</key><string>EventExplorer</string>
    <key>CFBundleDisplayName</key><string>EventExplorer</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleVersion</key><string>${VERSION}</string>
    <key>CFBundleIconFile</key><string>EventExplorer.icns</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
EOF
plutil -lint "$APP/Contents/Info.plist"
# Stripping changes the linker signature. Re-sign after all bundle edits;
# ad-hoc signing also allows local Apple Silicon launches without an identity.
codesign --force --sign - "$APP"
codesign --verify --strict "$APP"
echo "==> macOS app: $APP"
