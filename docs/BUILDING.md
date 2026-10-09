# Building Venturi from source

Prebuilt AppImages (x86_64, aarch64) and a macOS dmg are on the
[Releases](https://github.com/VenturiVideo/Venturi/releases) page. This
document covers building, testing and packaging from the repository.

## Dependencies

- **Rust** 1.88 or newer (edition 2024, let chains) — [rustup.rs](https://rustup.rs)
- **FFmpeg** (development headers/libs + `pkg-config`, for `ffmpeg-next`, the
  binding used for decode/encode)
- **clang/libclang** (for `ffmpeg-next`'s bindgen)
- **Wayland/X11 + Vulkan** (for `eframe`/`wgpu`, the UI and the GPU compositor)
- **ALSA** (for `cpal`, the audio)

On Fedora (including Asahi Remix):

```sh
sudo dnf install \
  rust cargo \
  ffmpeg ffmpeg-devel \
  clang clang-devel \
  wayland-devel libxkbcommon-devel libxkbcommon-x11 libX11-devel \
  vulkan-loader-devel mesa-vulkan-drivers \
  alsa-lib-devel
```

`ffmpeg-devel` on Fedora requires the RPM Fusion (free) repository to be
enabled — Fedora's own system build of FFmpeg does not include libx264 for
licensing reasons, but this project needs it both for decoding common H.264
sources and for export/proxies.

On Debian/Ubuntu the equivalents (translated from the Fedora packages
above, less regularly tested):

```sh
sudo apt install \
  build-essential pkg-config \
  libavcodec-dev libavformat-dev libavutil-dev libswscale-dev libswresample-dev \
  clang libclang-dev \
  libwayland-dev libxkbcommon-dev libx11-dev \
  libvulkan-dev mesa-vulkan-drivers \
  libasound2-dev
```

### libav version

If your system has a different version than the one linked against, e.g.:

```
error while loading shared libraries: libavutil.so.60: cannot open shared object file: No such file or directory
```

you can force a specific `libavutil` version through the `LD_LIBRARY_PATH`
environment variable, e.g.:

```
export LD_LIBRARY_PATH="/path/to/local/library/:$LD_LIBRARY_PATH"
./vv-app
```

to use a different local version.

## Build

From the workspace root:

```sh
cargo build -p vv-app            # debug
cargo build -p vv-app --release  # release (optimised, much slower to compile)
```

The first build is slow (`ffmpeg-next`'s bindgen + compiling `wgpu`);
subsequent ones are incremental.

## Run

```sh
cargo run -p vv-app
```

A working graphics backend is required at runtime (Vulkan on Linux, through
`mesa-vulkan-drivers` or the proprietary GPU driver): without one, `wgpu`
finds no adapter and the window does not open.

`vv-app mcp` runs the MCP server for AI agents instead of the window, and
`vv-app --mcp` opens the window with it on: see [MCP.md](MCP.md).

To install the binary, the `.desktop` file, the icons and the `.vvproj` MIME
type (so the file manager opens projects with Venturi) into `~/.local` (or
`/usr/local` as root):

```sh
scripts/install-linux.sh              # --uninstall to remove, --prefix DIR to change
```

## Test

```sh
cargo test -p vv-app
```

Tests run single-threaded (`RUST_TEST_THREADS=1` in `.cargo/config.toml`):
the graphics tests share headless GPU resources and SIGSEGV intermittently
when run in parallel. Some tests invoke `ffmpeg` from the command line to
generate synthetic clips in `/tmp`, so the `ffmpeg` binary (not just the
development libraries) also needs to be in `PATH`.

### Where unit tests live

Unlike the usual Rust convention (an inline `mod tests { … }` at the bottom
of each file), unit tests are kept out of the source files, in
`crates/<crate>/src/tests/`, mirroring the source path
(`src/otio/import.rs` → `src/tests/otio/import.rs`). Each source file pulls
its tests in as a child module, so they still see private items:

```rust
#[cfg(test)]
#[path = "tests/export.rs"]
mod tests;
```

Why: production code and tests can be measured separately with plain
line counters, e.g.
`cloc crates --include-lang=Rust --exclude-dir=tests`.

Consequences to keep in mind:
- a file getting its first tests needs the `#[path]` declaration above
  (from a nested module the path climbs up, e.g. `"../tests/otio/import.rs"`);
- renaming or moving a source file means updating its `#[path]` too;
- `src/tests/` holds unit tests, not integration tests (those would go in
  `crates/<crate>/tests/`, next to `src/`);
- tools that walk `src/` must expect the `tests/` subdirectory.

## Lint

```sh
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Headless test container

[`container/`](../container/README.md) contains a Podman environment to
build/run/screenshot vv-app without a real graphical session (Xvfb + software
Vulkan) — useful for checking the UI in isolation, or from a machine without a
display.

## Packaging

### AppImage

```sh
scripts/build-appimage.sh
```

Produces `target/appimage/Venturi-<arch>.AppImage` with FFmpeg compiled from
source inside it (shared libraries, with libx264, librubberband, zlib,
NVENC, Vulkan encode and the encoders of the voiceover takes: libmp3lame, libopus,
libvorbis): the target machine needs neither FFmpeg nor RPM Fusion.
glibc, ALSA, Vulkan and the NVIDIA driver, if any (NVENC requires >= 550),
stay the system ones.

The script re-runs itself inside a Debian 13 container
(`container/Containerfile.appimage`, built on first use): a glibc binary only
runs on a glibc >= the build one, so the release must not be compiled on
Fedora. All the host needs is `podman`.

FFmpeg is compiled once inside the build volume
(`venturi-appimage-target`); to start over from scratch:
`podman volume rm venturi-appimage-cargo venturi-appimage-target`,
and `podman rmi venturi-appimage` if you change `Containerfile.appimage`.

The `Release` GitHub Actions workflow (`.github/workflows/release.yml`) builds
the same image and runs the same script on x86_64 and aarch64 runners, see
[Releases](#releases).

The AppImage is GPL (it includes libx264) and does not include FDK-AAC:
export uses FFmpeg's native AAC encoder.

### macOS (Apple Silicon)

```sh
scripts/build-macos.sh
```

Must run on a Mac with the Xcode command line tools and `pkgconf`. Produces
`target/macos/Venturi.app` and `target/macos/Venturi-arm64.dmg`, with FFmpeg
(libx264, librubberband, zlib, libmp3lame, libopus, libvorbis,
VideoToolbox) bundled in
`Contents/Frameworks`.

The `Release` GitHub Actions workflow runs the same script on a `macos-14`
runner, see [Releases](#releases).

#### Opening the app on another Mac

The app is signed ad-hoc only, not notarized: once downloaded (browser,
AirDrop, Nextcloud…) macOS quarantines it and refuses to launch it, with
"The application Venturi can't be opened" or, from the terminal,
`zsh: operation not permitted`. Distribute the `.dmg`, not the bare `.app`
folder: zips and FAT/exFAT drives can drop the executable bit.

1. Remove the quarantine and launch:
   ```sh
   xattr -dr com.apple.quarantine /Applications/Venturi.app
   open /Applications/Venturi.app
   ```
2. If `xattr` also answers `operation not permitted`: System Settings →
   Privacy & Security → App Management, enable Terminal and repeat step 1.
   Alternatively, copy the app to the Desktop, remove the quarantine there and
   move it to `/Applications` afterwards.
3. Without a terminal: try to open the app once, then System Settings →
   Privacy & Security → "Open Anyway" at the bottom. Right click → Open no
   longer bypasses Gatekeeper since macOS Sequoia.

If it still does not start:
```sh
ls -l /Applications/Venturi.app/Contents/MacOS/vv-app   # needs the x bit
codesign -vvv --deep /Applications/Venturi.app          # signature intact?
/Applications/Venturi.app/Contents/MacOS/vv-app         # real startup error
```
A missing `x` is fixed with `chmod +x` on that file, a broken signature with
`codesign --force --deep --sign - /Applications/Venturi.app`.

### Releases

`.github/workflows/release.yml` builds the AppImages (x86_64, aarch64) and the
macOS dmg. Pushing a `v*` tag also publishes a GitHub release with the three
files attached and notes generated from the commits:

```sh
scripts/release.sh 0.2.0          # bumps Cargo.toml/Cargo.lock, commits, tags
git push origin master v0.2.0
```

The workflow refuses a tag that does not match the Cargo version.

Run manually from the Actions tab, it only builds and leaves the files as
workflow artifacts.
