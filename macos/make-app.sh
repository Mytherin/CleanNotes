#!/bin/bash
# Makes CleanNotes.app (for Apple silicon and Intel Macs) and a disk image
# with it, in dist/. Run on a Mac with rustup:
#
#   macos/make-app.sh
#
# The app is signed ad hoc only (no Apple Developer ID, so not notarized):
# the first time, open it with right-click → Open.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
DIST="$ROOT/dist"
APP="$DIST/CleanNotes.app"
VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)

export MACOSX_DEPLOYMENT_TARGET=11.0
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
rustup target add "${TARGETS[@]}" >/dev/null
for target in "${TARGETS[@]}"; do
    cargo build --release --bin CleanNotes --target "$target"
done

rm -rf "$APP" "$DIST/CleanNotes.dmg"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create \
    "target/${TARGETS[0]}/release/CleanNotes" \
    "target/${TARGETS[1]}/release/CleanNotes" \
    -output "$APP/Contents/MacOS/CleanNotes"

# The icon, from the iPhone app's (the same drawing).
ICONSET="$DIST/CleanNotes.iconset"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
SRC=ios/CleanNotes/Assets.xcassets/AppIcon.appiconset/AppIcon.png
for size in 16 32 128 256 512; do
    sips -z $size $size "$SRC" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) "$SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/CleanNotes.icns"
rm -rf "$ICONSET"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>CleanNotes</string>
    <key>CFBundleDisplayName</key><string>CleanNotes</string>
    <key>CFBundleIdentifier</key><string>com.cleannotes.CleanNotes.mac</string>
    <key>CFBundleExecutable</key><string>CleanNotes</string>
    <key>CFBundleIconFile</key><string>CleanNotes</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$APP"

# The disk image: the app, and Applications to drag it to.
STAGE="$DIST/dmg"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname CleanNotes -srcfolder "$STAGE" -ov -format UDZO "$DIST/CleanNotes.dmg" >/dev/null
rm -rf "$STAGE"
echo "Made $APP and $DIST/CleanNotes.dmg"
