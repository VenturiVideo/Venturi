#!/bin/bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

for arch in x86_64 aarch64; do
    info="$(bash scripts/appimage-update-info.sh "$arch")"
    test "$info" = "gh-releases-zsync|VenturiVideo|Venturi|latest|Venturi-$arch.AppImage.zsync"
done
if bash scripts/appimage-update-info.sh armv7 >/dev/null 2>&1; then
    echo "Unsupported architecture was accepted" >&2
    exit 1
fi
echo "AppImage update metadata checks passed"
