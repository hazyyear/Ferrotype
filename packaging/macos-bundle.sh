#!/usr/bin/env bash
# Wrap target/release/ferrotype in ferrotype.app and put it in dist/ferrotype-<version>-macos.dmg.
# Usage: packaging/macos-bundle.sh VERSION   (run from the repository root after `cargo build --release`)
set -euo pipefail
v="${1:?version}"
app="dist/ferrotype.app"
rm -rf "$app" && mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/ferrotype "$app/Contents/MacOS/ferrotype"
cp assets/brand/ferrotype.icns "$app/Contents/Resources/ferrotype.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>ferrotype</string>
  <key>CFBundleDisplayName</key><string>ferrotype</string>
  <key>CFBundleIdentifier</key><string>app.ferrotype</string>
  <key>CFBundleExecutable</key><string>ferrotype</string>
  <key>CFBundleIconFile</key><string>ferrotype</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${v}</string>
  <key>CFBundleVersion</key><string>${v}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleDocumentTypes</key><array><dict>
    <key>CFBundleTypeName</key><string>Photo</string>
    <key>CFBundleTypeRole</key><string>Editor</string>
    <key>LSHandlerRank</key><string>Alternate</string>
    <key>LSItemContentTypes</key><array><string>public.image</string><string>public.camera-raw-image</string></array>
  </dict></array>
</dict></plist>
PLIST
# Ad-hoc signature so Apple Silicon runs it (Gatekeeper still asks on first open: right-click > Open).
codesign --force --deep --sign - "$app"
hdiutil create -volname ferrotype -srcfolder "$app" -ov -format UDZO "dist/ferrotype-${v}-macos.dmg"
rm -rf "$app"
