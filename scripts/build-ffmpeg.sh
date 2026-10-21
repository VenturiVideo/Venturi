#!/bin/bash
# Builds FFmpeg from source (shared libraries) with libx264, librubberband,
# zlib, the encoders of the voiceover takes (libmp3lame, libopus, libvorbis)
# and the platform hardware encoder and decoders (NVENC, NVDEC and Vulkan
# video on Linux, VideoToolbox on macOS) into a local prefix. Used by build-appimage.sh and build-macos.sh.
#
# Usage: scripts/build-ffmpeg.sh <prefix>
set -euo pipefail

# No `realpath -m`: macOS has the BSD one.
mkdir -p "$1"
PREFIX="$(cd "$1" && pwd)"

FFMPEG_TAG="n9.0.1"
# NVENC SDK 12.2: an NVIDIA driver >= 550 is enough.
NV_HEADERS_TAG="n12.2.72.0"
X264_BRANCH="stable"
RUBBERBAND_TAG="v4.0.0"
LAME_VERSION="3.100"
OGG_VERSION="1.3.5"
VORBIS_VERSION="1.3.7"
OPUS_VERSION="1.5.2"
AUDIO_CODECS="lame-$LAME_VERSION ogg-$OGG_VERSION vorbis-$VORBIS_VERSION opus-$OPUS_VERSION"

OS="$(uname -s)"
if [ "$OS" = Darwin ]; then
    WANT="$FFMPEG_TAG videotoolbox $X264_BRANCH $RUBBERBAND_TAG $AUDIO_CODECS ${MACOSX_DEPLOYMENT_TARGET:-}"
    JOBS="$(sysctl -n hw.ncpu)"
    HW_FLAGS=(--enable-videotoolbox)
else
    WANT="$FFMPEG_TAG $NV_HEADERS_TAG nvdec vulkan $X264_BRANCH $RUBBERBAND_TAG $AUDIO_CODECS"
    JOBS="$(nproc)"
    # No VA-API: libva would be linked, not loaded with dlopen like CUDA and
    # Vulkan, and the AppImage would not start on a system without it.
    HW_FLAGS=(--enable-ffnvcodec --enable-nvenc --enable-cuda --enable-nvdec --enable-vulkan)
fi

STAMP="$PREFIX/.build-stamp"
if [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$WANT" ]; then
    exit 0
fi

rm -rf "$PREFIX"
SRC="$PREFIX/src"
mkdir -p "$SRC"
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"

if [ "$OS" != Darwin ]; then
    git clone --depth 1 --branch "$NV_HEADERS_TAG" \
        https://github.com/FFmpeg/nv-codec-headers.git "$SRC/nv-codec-headers"
    make -C "$SRC/nv-codec-headers" PREFIX="$PREFIX" install
fi

git clone --depth 1 --branch "$X264_BRANCH" \
    https://code.videolan.org/videolan/x264.git "$SRC/x264"
(
    cd "$SRC/x264"
    ./configure --prefix="$PREFIX" --enable-shared --enable-pic --disable-cli
    make -j"$JOBS"
    make install
)

# Needed by the `rubberband` filter of the fast playback audio.
# --libdir=lib: Debian's meson would pick lib/<triplet>.
git clone --depth 1 --branch "$RUBBERBAND_TAG" \
    https://github.com/breakfastquay/rubberband.git "$SRC/rubberband"
(
    cd "$SRC/rubberband"
    if [ "$OS" = Darwin ]; then
        # v4.0.0 uses size_t without <cstddef>; recent libc++ no longer
        # pulls it in transitively.
        export CXXFLAGS="${CXXFLAGS:-} -include cstddef"
    fi
    meson setup build --prefix="$PREFIX" --libdir=lib --buildtype=release \
        -Ddefault_library=shared -Dresampler=builtin \
        -Djni=disabled -Dladspa=disabled -Dvamp=disabled \
        -Dcmdline=disabled -Dtests=disabled
    meson install -C build
)

# The release tarballs, not the git repositories: they ship a ready
# `configure`, so no autotools are needed.
tarball() {
    curl -fsSL "$1" | tar xz -C "$SRC"
}
autotools_install() {
    (
        cd "$SRC/$1"
        shift
        ./configure --prefix="$PREFIX" --libdir="$PREFIX/lib" \
            --enable-shared --disable-static "$@"
        make -j"$JOBS"
        make install
    )
}

tarball "https://downloads.sourceforge.net/project/lame/lame/$LAME_VERSION/lame-$LAME_VERSION.tar.gz"
# Exported but not defined in 3.100 (the decoder ones once it is disabled):
# the macOS linker refuses them.
sed -i.orig -E '/^(lame_init_old|hip_.*|lame_decode.*)$/d' \
    "$SRC/lame-$LAME_VERSION/include/libmp3lame.sym"
autotools_install "lame-$LAME_VERSION" --disable-frontend --disable-decoder

tarball "https://downloads.xiph.org/releases/ogg/libogg-$OGG_VERSION.tar.gz"
autotools_install "libogg-$OGG_VERSION"

tarball "https://downloads.xiph.org/releases/vorbis/libvorbis-$VORBIS_VERSION.tar.gz"
# A PowerPC-era flag Apple's clang no longer knows.
sed -i.orig 's/-force_cpusubtype_ALL//g' "$SRC/libvorbis-$VORBIS_VERSION/configure"
autotools_install "libvorbis-$VORBIS_VERSION" --disable-examples

tarball "https://downloads.xiph.org/releases/opus/opus-$OPUS_VERSION.tar.gz"
autotools_install "opus-$OPUS_VERSION" --disable-doc --disable-extra-programs

git clone --depth 1 --branch "$FFMPEG_TAG" \
    https://github.com/FFmpeg/FFmpeg.git "$SRC/ffmpeg"
(
    cd "$SRC/ffmpeg"
    if [ "$OS" = Darwin ]; then
        # configure links rubberband with -lstdc++, absent from the macOS SDK.
        sed -i '' 's/-lstdc++/-lc++/g' configure
    fi
    # --disable-autodetect: no dependencies picked up at random from the build
    # machine; the ones we need are enabled explicitly.
    # LAME has no pkg-config file: its headers and library are found by path.
    ./configure --prefix="$PREFIX" --enable-shared --disable-static \
        --enable-gpl --disable-autodetect --disable-programs --disable-doc \
        --extra-cflags="-I$PREFIX/include" --extra-ldflags="-L$PREFIX/lib" \
        --enable-libx264 --enable-librubberband --enable-zlib \
        --enable-libmp3lame --enable-libopus --enable-libvorbis "${HW_FLAGS[@]}"
    make -j"$JOBS"
    make install
)

rm -rf "$SRC"
echo "$WANT" > "$STAMP"
