#!/usr/bin/env bash
# bundle-linux.sh — Build TunnelDesk and package it as a DEB package.
#
# Usage:
#   ./scripts/bundle-linux.sh [--version X.Y.Z] [--target jammy|noble]
#
# Options:
#   --version X.Y.Z      Override the version string (default: value in Cargo.toml)
#   --target jammy|noble Ubuntu release codename (default: noble, i.e., Ubuntu 24.04)
#
# Prerequisites (on the build machine):
#   - Rust toolchain (cargo, rustup)
#   - Node.js 24
#   - cargo-deb: cargo install cargo-deb
#   - libwebkit2gtk-4.1-dev, libsoup-3.0-dev, libjavascriptcoregtk-4.1-dev, libgtk-3-dev

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$SCRIPT_DIR/.."
cd "$ROOT"

APP_NAME="tunneldesk"
VERSION="0.1.0"
TARGET="noble"  # Ubuntu 24.04 default

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)     VERSION="$2"; shift 2 ;;
        --target)      TARGET="$2"; shift 2 ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

# Map codename to Debian architecture and dependency versions
case "$TARGET" in
    jammy)  # Ubuntu 22.04
        DEB_DEPENDENCIES="libwebkit2gtk-4.1-0 (>= 2.34), libsoup-3.0-0 (>= 3.0), libjavascriptcoregtk-4.1-0 (>= 2.34), libgtk-3-0 (>= 3.24)"
        ;;
    noble)  # Ubuntu 24.04
        DEB_DEPENDENCIES="libwebkit2gtk-4.1-0 (>= 2.42), libsoup-3.0-0 (>= 3.4), libjavascriptcoregtk-4.1-0 (>= 2.42), libgtk-3-0 (>= 3.24)"
        ;;
    *)
        echo "Error: Unsupported target '$TARGET'. Use 'jammy' (22.04) or 'noble' (24.04)" >&2
        exit 1
        ;;
esac

echo "==> Building frontend"
# Activate nvm if available
if [ -f "$HOME/.nvm/nvm.sh" ]; then
    source "$HOME/.nvm/nvm.sh"
    nvm use 24 2>/dev/null || true
fi
(cd frontend && npm ci && npm run build)

echo "==> Building Rust binary"
cargo build --release

echo "==> Installing cargo-deb if not present"
if ! command -v cargo-deb &>/dev/null; then
    cargo install cargo-deb
fi

echo "==> Installing ImageMagick for icon conversion if not present"
if ! command -v convert &>/dev/null; then
    sudo apt-get install -y imagemagick
fi

echo "==> Creating DEB package for Ubuntu $TARGET"
# Create a temporary deb manifest directory
DEB_DIR="$ROOT/target/deb"
rm -rf "$DEB_DIR"
mkdir -p "$DEB_DIR/DEBIAN"
mkdir -p "$DEB_DIR/usr/bin"
mkdir -p "$DEB_DIR/usr/share/doc/$APP_NAME"
mkdir -p "$DEB_DIR/etc/$APP_NAME"
mkdir -p "$DEB_DIR/usr/share/applications"
mkdir -p "$DEB_DIR/usr/share/icons/hicolor/16x16/apps"
mkdir -p "$DEB_DIR/usr/share/icons/hicolor/32x32/apps"
mkdir -p "$DEB_DIR/usr/share/icons/hicolor/48x48/apps"
mkdir -p "$DEB_DIR/usr/share/icons/hicolor/64x64/apps"
mkdir -p "$DEB_DIR/usr/share/icons/hicolor/128x128/apps"
mkdir -p "$DEB_DIR/usr/share/icons/hicolor/256x256/apps"
mkdir -p "$DEB_DIR/usr/share/icons/hicolor/512x512/apps"
mkdir -p "$DEB_DIR/usr/share/icons/hicolor/scalable/apps"

# Copy binary
cp "$ROOT/target/release/$APP_NAME" "$DEB_DIR/usr/bin/$APP_NAME"
chmod +x "$DEB_DIR/usr/bin/$APP_NAME"

# Copy documentation
cp "$ROOT/README.md" "$DEB_DIR/usr/share/doc/$APP_NAME/"
cp "$ROOT/LICENSE" "$DEB_DIR/usr/share/doc/$APP_NAME/" 2>/dev/null || true

# Copy .desktop file
cp "$ROOT/dist/linux/tunneldesk.desktop" "$DEB_DIR/usr/share/applications/"
chmod +x "$DEB_DIR/usr/share/applications/tunneldesk.desktop"

# Convert and copy icons in multiple sizes
echo "==> Converting icons"
ICON_SOURCE="$ROOT/logo.svg"
if [ -f "$ICON_SOURCE" ]; then
    # Convert SVG to various PNG sizes
    convert "$ICON_SOURCE" -resize 16x16 "$DEB_DIR/usr/share/icons/hicolor/16x16/apps/tunneldesk.png"
    convert "$ICON_SOURCE" -resize 32x32 "$DEB_DIR/usr/share/icons/hicolor/32x32/apps/tunneldesk.png"
    convert "$ICON_SOURCE" -resize 48x48 "$DEB_DIR/usr/share/icons/hicolor/48x48/apps/tunneldesk.png"
    convert "$ICON_SOURCE" -resize 64x64 "$DEB_DIR/usr/share/icons/hicolor/64x64/apps/tunneldesk.png"
    convert "$ICON_SOURCE" -resize 128x128 "$DEB_DIR/usr/share/icons/hicolor/128x128/apps/tunneldesk.png"
    convert "$ICON_SOURCE" -resize 256x256 "$DEB_DIR/usr/share/icons/hicolor/256x256/apps/tunneldesk.png"
    convert "$ICON_SOURCE" -resize 512x512 "$DEB_DIR/usr/share/icons/hicolor/512x512/apps/tunneldesk.png"
    # Copy SVG for scalable version
    cp "$ICON_SOURCE" "$DEB_DIR/usr/share/icons/hicolor/scalable/apps/tunneldesk.svg"
else
    echo "Warning: Icon file not found at $ICON_SOURCE"
fi

# Create control file
cat > "$DEB_DIR/DEBIAN/control" <<EOF
Package: $APP_NAME
Version: $VERSION
Section: utils
Priority: optional
Architecture: amd64
Maintainer: Heiko Rothkranz <heiko@rothkranz.net>
Depends: $DEB_DEPENDENCIES
Description: A local HTTP proxy for Cloudflare Tunnels with request inspection
 TunnelDesk is a local HTTP proxy for Cloudflare Tunnels with request
 inspection and WebSocket support. It provides a GUI window and
 can manage multiple tunnels with automatic Cloudflare integration.
 .
 Note: cloudflared must be installed separately for full functionality.
EOF

# Create postinst script to handle desktop database update
cat > "$DEB_DIR/DEBIAN/postinst" <<'EOF'
#!/bin/bash
set -e

case "$1" in
    configure)
        # Update desktop database so the app appears in the menu
        if command -v update-desktop-database &>/dev/null; then
            update-desktop-database /usr/share/applications || true
        fi
        # Update icon cache
        if command -v gtk-update-icon-cache &>/dev/null; then
            gtk-update-icon-cache -f -t /usr/share/icons/hicolor || true
        fi
        echo "TunnelDesk installed successfully."
        echo "To install cloudflared service, run: cloudflared service install <token>"
        ;;
esac

exit 0
EOF
chmod +x "$DEB_DIR/DEBIAN/postinst"

# Create prerm script
cat > "$DEB_DIR/DEBIAN/prerm" <<'EOF'
#!/bin/bash
set -e

case "$1" in
    remove|upgrade|deconfigure)
        # Stop any running instances
        pkill -x tunneldesk 2>/dev/null || true
        # Update desktop database
        if command -v update-desktop-database &>/dev/null; then
            update-desktop-database /usr/share/applications || true
        fi
        # Update icon cache
        if command -v gtk-update-icon-cache &>/dev/null; then
            gtk-update-icon-cache -f -t /usr/share/icons/hicolor || true
        fi
        ;;
esac

exit 0
EOF
chmod +x "$DEB_DIR/DEBIAN/prerm"

# Calculate installed size
INSTALLED_SIZE=$(du -sk "$DEB_DIR" | cut -f1)
echo "Installed-Size: $INSTALLED_SIZE" >> "$DEB_DIR/DEBIAN/control"

# Build the DEB package
DEB_FILE="$ROOT/${APP_NAME}_${VERSION}_amd64_${TARGET}.deb"
dpkg-deb --build "$DEB_DIR" "$DEB_FILE"

# Clean up
rm -rf "$DEB_DIR"

echo ""
echo "Done: $DEB_FILE"
echo ""
echo "To install:"
echo "  sudo dpkg -i $DEB_FILE"
echo "  sudo apt-get install -f  # to install dependencies if needed"
echo ""
echo "Note: cloudflared must be installed separately:"
echo "  wget -q https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64.deb"
echo "  sudo dpkg -i cloudflared-linux-amd64.deb"
