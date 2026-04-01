#!/usr/bin/env bash
# bundle-macos.sh — Build TunnelDesk and package it as a macOS .app bundle.
#
# Usage:
#   ./scripts/bundle-macos.sh [--universal] [--version X.Y.Z]
#                              [--sign "Developer ID Application: ..."]
#
# Options:
#   --universal          Build a universal binary (arm64 + x86_64) via lipo.
#                        Requires both targets to be installed:
#                          rustup target add aarch64-apple-darwin x86_64-apple-darwin
#   --version X.Y.Z      Override the version string embedded in the bundle and
#                        used as the DMG filename (default: value in script).
#   --sign IDENTITY      Code-sign with the given Developer ID identity.
#                        Pass the full string from `security find-identity -v -p codesigning`.
#   --notarize PROFILE   Notarize after signing using the given notarytool profile name.
#                        Set up with: xcrun notarytool store-credentials <PROFILE>
#
# Prerequisites (on the build machine):
#   - Rust toolchain (cargo, rustup)
#   - Node.js 24 via nvm  (or on PATH already)
#   - Xcode Command Line Tools (for codesign / xcrun)
#   - create-dmg (optional, for DMG output): brew install create-dmg

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$SCRIPT_DIR/.."
cd "$ROOT"

APP_NAME="TunnelDesk"
BINARY_NAME="tunneldesk"
VERSION="0.1.0"
BUNDLE_ID="net.rothkranz.tunneldesk.app"

UNIVERSAL=false
SIGN_IDENTITY=""
NOTARIZE_PROFILE=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --universal)   UNIVERSAL=true; shift ;;
        --version)     VERSION="$2"; shift 2 ;;
        --sign)        SIGN_IDENTITY="$2"; shift 2 ;;
        --notarize)    NOTARIZE_PROFILE="$2"; shift 2 ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

echo "==> Building frontend"
# Activate nvm if available
if [ -f "$HOME/.nvm/nvm.sh" ]; then
    source "$HOME/.nvm/nvm.sh"
    nvm use 24 2>/dev/null || true
fi
(cd frontend && npm ci && npm run build)

echo "==> Building Rust binary"
if $UNIVERSAL; then
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
    cargo build --release --target aarch64-apple-darwin
    cargo build --release --target x86_64-apple-darwin
    BINARY_PATH="$ROOT/target/universal-release/$BINARY_NAME"
    mkdir -p "$ROOT/target/universal-release"
    lipo -create -output "$BINARY_PATH" \
        "$ROOT/target/aarch64-apple-darwin/release/$BINARY_NAME" \
        "$ROOT/target/x86_64-apple-darwin/release/$BINARY_NAME"
    echo "    Universal binary: $(lipo -archs "$BINARY_PATH")"
else
    cargo build --release
    BINARY_PATH="$ROOT/target/release/$BINARY_NAME"
fi

echo "==> Assembling $APP_NAME.app"
APP_DIR="$ROOT/$APP_NAME.app"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"

cp "$BINARY_PATH"              "$APP_DIR/Contents/MacOS/$BINARY_NAME"
cp dist/macos/Info.plist       "$APP_DIR/Contents/Info.plist"

# Stamp version into the bundle's Info.plist
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APP_DIR/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $VERSION"            "$APP_DIR/Contents/Info.plist"

# Copy app icon if present
if [ -f "dist/macos/AppIcon.icns" ]; then
    cp dist/macos/AppIcon.icns "$APP_DIR/Contents/Resources/AppIcon.icns"
fi

echo "==> Bundle layout"
find "$APP_DIR" -type f

if [ -n "$SIGN_IDENTITY" ]; then
    echo "==> Signing with: $SIGN_IDENTITY"
    codesign --deep --force --options runtime \
        --entitlements dist/macos/entitlements.plist \
        --sign "$SIGN_IDENTITY" \
        "$APP_DIR"
    codesign --verify --deep --strict --verbose=2 "$APP_DIR"
    echo "    Signature OK"
fi

echo "==> Creating $APP_NAME-$VERSION.dmg"
if command -v create-dmg &>/dev/null; then
    create-dmg \
        --volname "$APP_NAME $VERSION" \
        --window-size 540 380 \
        --icon-size 128 \
        --icon "$APP_NAME.app" 130 160 \
        --app-drop-link 400 160 \
        "$ROOT/$APP_NAME-$VERSION.dmg" \
        "$APP_DIR"
else
    # Fallback: plain hdiutil DMG (no drag-to-Applications UI)
    DMG_STAGE="$ROOT/dmg-stage"
    rm -rf "$DMG_STAGE"
    mkdir "$DMG_STAGE"
    cp -r "$APP_DIR" "$DMG_STAGE/"
    hdiutil create -volname "$APP_NAME $VERSION" \
        -srcfolder "$DMG_STAGE" \
        -ov -format UDZO \
        "$ROOT/$APP_NAME-$VERSION.dmg"
    rm -rf "$DMG_STAGE"
fi

if [ -n "$SIGN_IDENTITY" ]; then
    echo "==> Signing DMG"
    codesign --sign "$SIGN_IDENTITY" "$ROOT/$APP_NAME-$VERSION.dmg"
fi

if [ -n "$NOTARIZE_PROFILE" ]; then
    echo "==> Notarizing (profile: $NOTARIZE_PROFILE)"
    xcrun notarytool submit "$ROOT/$APP_NAME-$VERSION.dmg" \
        --keychain-profile "$NOTARIZE_PROFILE" \
        --wait
    xcrun stapler staple "$ROOT/$APP_NAME-$VERSION.dmg"
    echo "    Notarization OK"
fi

echo ""
echo "Done: $ROOT/$APP_NAME-$VERSION.dmg"
echo ""
echo "To install: open the DMG, drag $APP_NAME to Applications."
echo "Note: cloudflared must be installed separately (brew install cloudflare/cloudflare/cloudflared)."
