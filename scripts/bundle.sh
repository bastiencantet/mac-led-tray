#!/bin/bash
# Build LED.app bundle, including the Sparkle.framework update engine.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_NAME="LED"
BUNDLE_ID="com.bastiencantet.led-tray"
VERSION="${VERSION:-0.1.0}"
APP_DIR="dist/${APP_NAME}.app"
APPCAST_URL="https://raw.githubusercontent.com/bastiencantet/mac-led-tray/main/appcast.xml"
SPARKLE_PUBLIC_KEY="$(cat "${ROOT}/.sparkle_public_key")"

echo "==> Building release binaries (version ${VERSION})"
cargo build --release --bin mac-led-tray
cargo build --release --bin led-helper

echo "==> Creating bundle: ${APP_DIR}"
rm -rf "${APP_DIR}"
mkdir -p "${APP_DIR}/Contents/MacOS"
mkdir -p "${APP_DIR}/Contents/Resources"
mkdir -p "${APP_DIR}/Contents/Frameworks"

cp "target/release/mac-led-tray" "${APP_DIR}/Contents/MacOS/"
cp "target/release/led-helper"  "${APP_DIR}/Contents/MacOS/"
chmod +x "${APP_DIR}/Contents/MacOS/mac-led-tray"
chmod +x "${APP_DIR}/Contents/MacOS/led-helper"

echo "==> Embedding Sparkle.framework"
cp -R "vendor/Sparkle.framework" "${APP_DIR}/Contents/Frameworks/"

cat > "${APP_DIR}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleExecutable</key>
    <string>mac-led-tray</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
    <key>SUFeedURL</key>
    <string>${APPCAST_URL}</string>
    <key>SUPublicEDKey</key>
    <string>${SPARKLE_PUBLIC_KEY}</string>
    <key>SUEnableAutomaticChecks</key>
    <true/>
    <key>SUScheduledCheckInterval</key>
    <integer>86400</integer>
</dict>
</plist>
PLIST

echo "==> Ad-hoc codesigning (Sparkle + app)"
# Sign the embedded framework first (recursively covers XPCServices inside).
codesign --force --deep --sign - "${APP_DIR}/Contents/Frameworks/Sparkle.framework"
codesign --force --sign - "${APP_DIR}/Contents/MacOS/led-helper"
codesign --force --sign - "${APP_DIR}/Contents/MacOS/mac-led-tray"
codesign --force --sign - "${APP_DIR}"

echo "==> Done: ${APP_DIR}"
