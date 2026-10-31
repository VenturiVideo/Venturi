#!/bin/bash
set -euo pipefail

case "${1:-}" in
    x86_64|aarch64)
        printf 'gh-releases-zsync|VenturiVideo|Venturi|latest|Venturi-%s.AppImage.zsync\n' "$1"
        ;;
    *)
        echo "Unsupported AppImage architecture: ${1:-missing}" >&2
        exit 2
        ;;
esac
