#!/bin/bash
# Installs vv-app, the .desktop file, the icons and the .vvproj MIME type into
# the given prefix.
#
# Usage: scripts/install-linux.sh [--uninstall] [--prefix DIR]
# Default: ~/.local, or /usr/local when run as root.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

PREFIX="${PREFIX:-}"
UNINSTALL=0
while [ $# -gt 0 ]; do
    case "$1" in
        --uninstall) UNINSTALL=1 ;;
        --prefix) PREFIX="$2"; shift ;;
        --prefix=*) PREFIX="${1#--prefix=}" ;;
        *) echo "unknown argument: $1" >&2; exit 1 ;;
    esac
    shift
done
if [ -z "$PREFIX" ]; then
    if [ "$(id -u)" -eq 0 ]; then PREFIX=/usr/local; else PREFIX="$HOME/.local"; fi
fi

BIN="$PREFIX/bin/venturi"
DESKTOP="$PREFIX/share/applications/venturi.desktop"
ICONS="$PREFIX/share/icons/hicolor"
MIME="$PREFIX/share/mime"

refresh_caches() {
    # Without this the menus keep showing the old entry/icon.
    command -v gtk-update-icon-cache >/dev/null && \
        gtk-update-icon-cache -q -t -f "$ICONS" 2>/dev/null || true
    command -v update-desktop-database >/dev/null && \
        update-desktop-database -q "$PREFIX/share/applications" 2>/dev/null || true
    command -v update-mime-database >/dev/null && [ -d "$MIME/packages" ] && \
        update-mime-database "$MIME" 2>/dev/null || true
}

if [ "$UNINSTALL" -eq 1 ]; then
    rm -f "$PREFIX/bin/vv-app" "$BIN" "$DESKTOP" "$MIME/packages/venturi.xml"
    for icon in "$ICONS"/*/apps/venturi.{png,svg}; do
        rm -f -- "$icon"
    done
    refresh_caches
    echo "removed from $PREFIX"
    exit 0
fi

TARGET_DIR="$(realpath -m "${CARGO_TARGET_DIR:-target}")"
if [ ! -x "$TARGET_DIR/release/vv-app" ]; then
    cargo build -p vv-app --release
fi

install -Dm755 "$TARGET_DIR/release/vv-app" "$BIN"
LEGACY_REPLACED=0
if [ -e "$PREFIX/bin/vv-app" ]; then
    rm -f "$PREFIX/bin/vv-app"
    LEGACY_REPLACED=1
fi
mkdir -p "$(dirname "$DESKTOP")"
source scripts/linux-desktop.sh
write_desktop_entry packaging/appimage/venturi.desktop "$BIN" > "$DESKTOP"
chmod 644 "$DESKTOP"
install -Dm644 packaging/venturi-mime.xml "$MIME/packages/venturi.xml"

# The icon file name must match the Icon= key of the .desktop.
for dir in media/icons/linux/hicolor/*/apps; do
    size="$(basename "$(dirname "$dir")")"
    for f in "$dir"/venturi-video.*; do
        install -Dm644 "$f" "$ICONS/$size/apps/venturi.${f##*.}"
    done
done

refresh_caches

echo "installed into $PREFIX"
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "note: $PREFIX/bin is not in PATH" ;;
esac
if [ "$LEGACY_REPLACED" -eq 1 ]; then
    echo "note: the command is now venturi, $PREFIX/bin/vv-app was removed;"
    echo "      point MCP configs at \"$BIN mcp\" (see docs/MCP.md)"
fi
