#!/bin/bash
# Venturi.app + .dmg for macOS (Apple Silicon), with FFmpeg (libx264,
# VideoToolbox) and libx264 bundled in Contents/Frameworks. Must run on a Mac
# with the Xcode command line tools; the app is signed ad-hoc only.
#
# Usage: scripts/build-macos.sh
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [ "$(uname -s)" != Darwin ]; then
    echo "build-macos.sh must run on macOS" >&2
    exit 1
fi

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"

ARCH="$(uname -m)"
mkdir -p "${CARGO_TARGET_DIR:-target}"
TARGET_DIR="$(cd "${CARGO_TARGET_DIR:-target}" && pwd)"
FFMPEG_PREFIX="$TARGET_DIR/ffmpeg-shared"
OUT_DIR="$TARGET_DIR/macos"
APP="$OUT_DIR/Venturi.app"
DMG="$OUT_DIR/Venturi-$ARCH.dmg"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"

scripts/build-ffmpeg.sh "$FFMPEG_PREFIX"

# Same reasons as in build-appimage.sh: a Homebrew ffmpeg must not win the
# link, and a cached vv-app would not relink against the new prefix.
export FFMPEG_DIR="$FFMPEG_PREFIX"
export PKG_CONFIG_PATH="$FFMPEG_PREFIX/lib/pkgconfig"
export RUSTFLAGS="-L native=$FFMPEG_PREFIX/lib${RUSTFLAGS:+ $RUSTFLAGS}"
cargo clean --release -p ffmpeg-sys-next -p vv-media -p vv-app
cargo build -p vv-app --release

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources"
cp "$TARGET_DIR/release/vv-app" "$APP/Contents/MacOS/"
cp media/icons/macos/venturi-video.icns "$APP/Contents/Resources/venturi.icns"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Venturi</string>
    <key>CFBundleDisplayName</key><string>Venturi</string>
    <key>CFBundleIdentifier</key><string>com.morrolinux.venturi</string>
    <key>CFBundleExecutable</key><string>vv-app</string>
    <key>CFBundleIconFile</key><string>venturi</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>$MACOSX_DEPLOYMENT_TARGET</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.video</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF

# The prefix dylibs carry absolute install names: each one is copied under
# its install name and every reference to a bundled library is rewritten to
# @rpath. Matched by basename, not by $FFMPEG_PREFIX: a cached prefix keeps
# the install names of the path it was built at (#4).
for lib in "$FFMPEG_PREFIX"/lib/*.dylib; do
    [ -L "$lib" ] && continue
    cp "$lib" "$APP/Contents/Frameworks/$(basename "$(otool -D "$lib" | tail -1)")"
done
relink() {
    local file="$1" dep
    for dep in $(otool -L "$file" | awk 'NR > 1 { print $1 }'); do
        case "$dep" in @*|/usr/lib/*|/System/*) continue ;; esac
        if [ -e "$APP/Contents/Frameworks/$(basename "$dep")" ]; then
            install_name_tool -change "$dep" "@rpath/$(basename "$dep")" "$file"
        fi
    done
}
for lib in "$APP"/Contents/Frameworks/*.dylib; do
    install_name_tool -id "@rpath/$(basename "$lib")" "$lib"
    relink "$lib"
done
relink "$APP/Contents/MacOS/vv-app"
install_name_tool -add_rpath "@executable_path/../Frameworks" "$APP/Contents/MacOS/vv-app"

# Every required @rpath library must be in the bundle and nothing may link
# an absolute path outside the system, which the user's machine lacks.
for file in "$APP"/Contents/MacOS/vv-app "$APP"/Contents/Frameworks/*.dylib; do
    for dep in $(otool -L "$file" | awk 'NR > 1 { print $1 }'); do
        case "$dep" in
            /usr/lib/*|/System/*) ;;
            @rpath/*)
                if [ ! -e "$APP/Contents/Frameworks/${dep#@rpath/}" ]; then
                    echo "required library missing from the bundle: $dep" >&2
                    exit 1
                fi ;;
            *)
                echo "$file links $dep, not available on the user's machine" >&2
                exit 1 ;;
        esac
    done
done

# install_name_tool invalidates the signatures, and arm64 refuses to run
# unsigned code: ad-hoc re-sign, libraries first.
codesign --force --sign - "$APP"/Contents/Frameworks/*.dylib
codesign --force --sign - "$APP"

rm -f "$DMG"
STAGE="$OUT_DIR/dmg"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname Venturi -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"

echo "App: $APP"
echo "DMG: $DMG"
