#!/bin/sh
set -eu

flatpak run --command=sh io.github.VenturiVideo.Venturi -c '
set -eu
libraries=$(ldd /app/bin/venturi)
if printf "%s\n" "$libraries" | grep -q "not found"; then
    printf "%s\n" "$libraries" >&2
    exit 1
fi
printf "%s\n" "$libraries" | grep -q "libavfilter.* /app/lib/"
filters=$(ffmpeg -hide_banner -filters 2>&1)
printf "%s\n" "$filters" | grep -q rubberband
encoders=$(ffmpeg -hide_banner -encoders 2>&1)
for codec in libx264 aac libmp3lame libopus libvorbis; do
    printf "%s\n" "$encoders" | grep -q "$codec"
done
test -f /app/share/applications/io.github.VenturiVideo.Venturi.desktop
test -f /app/share/metainfo/io.github.VenturiVideo.Venturi.metainfo.xml
test -f /app/share/mime/packages/io.github.VenturiVideo.Venturi.xml
test -f /app/share/icons/hicolor/scalable/apps/io.github.VenturiVideo.Venturi.svg
test_dir=$(mktemp -d)
trap '\''rm -rf "$test_dir"'\'' EXIT
ffmpeg -hide_banner -loglevel error -f lavfi -i testsrc2=size=64x64:rate=25:duration=1 \
    -f lavfi -i sine=frequency=440:sample_rate=48000:duration=1 \
    -c:v libx264 -threads 1 -af rubberband=tempo=1.25 -c:a aac -shortest "$test_dir/smoke.mp4"
ffprobe -v error -show_entries stream=codec_name,width,height -of json "$test_dir/smoke.mp4"
ffmpeg -v error -threads 1 -i "$test_dir/smoke.mp4" -f null -
printf "Installed package libraries, codecs, time stretch and file integration passed.\n"
'
