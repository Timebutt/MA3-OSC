#!/bin/sh
# Build target/release/MA3-OSC.app (the MA3-OSC binary in a macOS app).
#
#   macos/make-app.sh              build the app
#   macos/make-app.sh --install    build it and copy it to /Applications
#
# Signing: ad-hoc by default (runs on this Mac). For other Macs, set
# SIGN_IDENTITY="Developer ID Application: ..." and notarize the result.
# UNIVERSAL=1 builds for both Apple silicon and Intel (needs both rustup targets).
set -eu
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
APP=target/release/MA3-OSC.app

if [ "${UNIVERSAL:-0}" = 1 ]; then
    cargo build --release --target aarch64-apple-darwin
    cargo build --release --target x86_64-apple-darwin
    BIN=target/release/ma3-osc-universal
    lipo -create -output "$BIN" \
        target/aarch64-apple-darwin/release/ma3-osc target/x86_64-apple-darwin/release/ma3-osc
else
    cargo build --release
    BIN=target/release/ma3-osc
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/plugin"
cp "$BIN" "$APP/Contents/MacOS/ma3-osc"
sed "s/@VERSION@/$VERSION/g" macos/Info.plist > "$APP/Contents/Info.plist"
plutil -lint -s "$APP/Contents/Info.plist"

# starter files, copied to ~/Library/Application Support/MA3-OSC on first start
cp ma3-osc.json "$APP/Contents/Resources/"
cp -R mappings "$APP/Contents/Resources/"
cp plugin/ma3-osc.xml "$APP/Contents/Resources/plugin/"

# icon
ICONSET=target/release/AppIcon.iconset
rm -rf "$ICONSET" && mkdir -p "$ICONSET"
python3 macos/make_icon.py "$ICONSET/icon_512x512@2x.png"
for s in 16 32 128 256 512; do
    sips -z $s $s "$ICONSET/icon_512x512@2x.png" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    d=$((s * 2))
    [ $d -ne 1024 ] && sips -z $d $d "$ICONSET/icon_512x512@2x.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

ID="${SIGN_IDENTITY:--}"
if [ "$ID" = - ]; then
    codesign --force --sign - "$APP"
else
    codesign --force --options runtime --timestamp --sign "$ID" "$APP"
fi
codesign --verify --strict "$APP"
echo "built $APP ($VERSION)"

if [ "${1:-}" = --install ]; then
    rm -rf /Applications/MA3-OSC.app
    ditto "$APP" /Applications/MA3-OSC.app
    echo "installed /Applications/MA3-OSC.app (find it with Spotlight: \"MA3-OSC\")"
fi
