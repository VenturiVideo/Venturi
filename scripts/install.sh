#!/usr/bin/env bash
set -euo pipefail

# Release CI replaces this value so downloaded installers stay on their release.
DEFAULT_VERSION=latest
RELEASES=https://github.com/VenturiVideo/Venturi/releases

fail() {
    echo "Error: $*" >&2
    exit 1
}

if [[ ${1:-} == --help || ${1:-} == -h ]]; then
    cat <<'EOF'
Usage: bash install.sh [latest|vX.Y.Z|--uninstall]

Install the Linux AppImage and a menu entry for the current user.
Run again to replace the installed version. No sudo is needed.
Use --uninstall to remove the binary and desktop integration.

Environment:
  INSTALL_DIR    Binary directory (default: $HOME/.local/bin)
  XDG_DATA_HOME  Desktop data directory (default: $HOME/.local/share)
EOF
    exit 0
fi
[[ $# -le 1 ]] || fail "Usage: bash install.sh [latest|vX.Y.Z|--uninstall]"
VERSION="${1:-$DEFAULT_VERSION}"
[[ $VERSION == --uninstall || $VERSION == latest || $VERSION =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$ ]] \
    || fail "Expected latest or a release tag such as v0.2.0."
[[ $(uname -s) == Linux ]] || fail "This installer supports Linux. Download the macOS DMG from $RELEASES."
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
for directory in "$INSTALL_DIR" "$DATA_DIR"; do
    [[ $directory == /* && $directory != *$'\n'* && $directory != *$'\r'* ]] \
        || fail "Install directories must be absolute paths without line breaks."
done

refresh_caches() {
    command -v gtk-update-icon-cache >/dev/null && \
        gtk-update-icon-cache -q -t -f "$DATA_DIR/icons/hicolor" 2>/dev/null || true
    command -v update-desktop-database >/dev/null && \
        update-desktop-database "$DATA_DIR/applications" 2>/dev/null || true
    command -v update-mime-database >/dev/null && [[ -d $DATA_DIR/mime/packages ]] && \
        update-mime-database "$DATA_DIR/mime" 2>/dev/null || true
}

if [[ $VERSION == --uninstall ]]; then
    rm -f -- "$INSTALL_DIR/venturi" "$INSTALL_DIR/vv-app" "$DATA_DIR/applications/venturi.desktop" \
        "$DATA_DIR/mime/packages/venturi.xml"
    for icon in "$DATA_DIR"/icons/hicolor/*/apps/venturi.{png,svg}; do
        rm -f -- "$icon"
    done
    refresh_caches
    echo "Removed Venturi from $INSTALL_DIR and $DATA_DIR."
    exit 0
fi

ARCH="$(uname -m)"
case "$ARCH" in
    x86_64|aarch64) ;;
    *) fail "Unsupported Linux architecture: $ARCH" ;;
esac
command -v curl >/dev/null || fail "Install curl and try again."
command -v tar >/dev/null || fail "Install tar and try again."

if [[ $VERSION == latest ]]; then
    BASE_URL="$RELEASES/latest/download"
else
    BASE_URL="$RELEASES/download/$VERSION"
fi

mkdir -p "$INSTALL_DIR" "$DATA_DIR/applications"
STAGING="$(mktemp -d "$INSTALL_DIR/.venturi-install.XXXXXX")"
DESKTOP_TMP=
cleanup() {
    rm -rf -- "$STAGING"
    if [[ -n $DESKTOP_TMP ]]; then rm -f -- "$DESKTOP_TMP"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo "Downloading Venturi ($VERSION, $ARCH)..."
curl --fail --location --show-error --silent --retry 3 \
    --proto '=https' --proto-redir '=https' \
    --output "$STAGING/venturi" "$BASE_URL/Venturi-$ARCH.AppImage"
[[ -s "$STAGING/venturi" ]] || fail "The downloaded AppImage is empty."
curl --fail --location --show-error --silent --retry 3 \
    --proto '=https' --proto-redir '=https' \
    --output "$STAGING/desktop.tar.gz" "$BASE_URL/venturi-desktop.tar.gz" \
    || fail "This release has no desktop integration package; it predates the installer."
tar -xzf "$STAGING/desktop.tar.gz" -C "$STAGING" --no-same-owner
[[ -s $STAGING/packaging/appimage/venturi.desktop && -s $STAGING/packaging/venturi-mime.xml \
    && -s $STAGING/scripts/linux-desktop.sh ]] \
    || fail "The desktop integration package is incomplete."
chmod 755 "$STAGING/venturi"

# shellcheck source=scripts/linux-desktop.sh
source "$STAGING/scripts/linux-desktop.sh"
DESKTOP_TMP="$(mktemp "$DATA_DIR/applications/.venturi.XXXXXX")"
write_desktop_entry "$STAGING/packaging/appimage/venturi.desktop" "$INSTALL_DIR/venturi" > "$DESKTOP_TMP"
install -Dm644 "$STAGING/packaging/venturi-mime.xml" "$DATA_DIR/mime/packages/venturi.xml"
for icon in "$STAGING"/media/icons/linux/hicolor/*/apps/venturi-video.*; do
    size="$(basename "$(dirname "$(dirname "$icon")")")"
    install -Dm644 "$icon" "$DATA_DIR/icons/hicolor/$size/apps/venturi.${icon##*.}"
done
mv -fT -- "$STAGING/venturi" "$INSTALL_DIR/venturi"
LEGACY_REPLACED=false
if [[ -e $INSTALL_DIR/vv-app ]]; then
    rm -f -- "$INSTALL_DIR/vv-app"
    LEGACY_REPLACED=true
fi
chmod 644 "$DESKTOP_TMP"
mv -fT -- "$DESKTOP_TMP" "$DATA_DIR/applications/venturi.desktop"
refresh_caches

echo "Installed $INSTALL_DIR/venturi. Launch Venturi from your application menu."
case ":$PATH:" in
    *":$INSTALL_DIR:"*) echo "You can also run: venturi" ;;
    *) echo "Add $INSTALL_DIR to PATH to run venturi from the terminal." ;;
esac

echo "AppImages may require FUSE 2. Without it, run: venturi --appimage-extract-and-run"
if $LEGACY_REPLACED; then
    echo "The command is now venturi and $INSTALL_DIR/vv-app was removed."
    echo "Point MCP configs at \"$INSTALL_DIR/venturi mcp\" (see docs/MCP.md)."
fi
