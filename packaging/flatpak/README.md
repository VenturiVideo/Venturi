# Flatpak development package

This manifest builds a pinned Venturi snapshot with FFmpeg, x264, Rubber Band
and the recording encoders. It is preparation for #90, not an existing Flathub
release. The freedesktop 26.08 runtime provides the desktop and Vulkan stack.

Install `flatpak` and `flatpak-builder`, add the Flathub remote, then install:

```sh
flatpak install flathub org.freedesktop.Platform//26.08 org.freedesktop.Sdk//26.08 \
  org.freedesktop.Sdk.Extension.rust-stable//26.08 \
  org.freedesktop.Sdk.Extension.llvm22//26.08
```

From this directory:

```sh
jobs=$(($(getconf _NPROCESSORS_ONLN) - 2))
[ "$jobs" -ge 1 ] || jobs=1
flatpak-builder --user --jobs="$jobs" --repo=repo build io.github.VenturiVideo.Venturi.json
flatpak --user remote-add --if-not-exists --no-gpg-verify venturi-local repo
flatpak --user install venturi-local io.github.VenturiVideo.Venturi
flatpak run io.github.VenturiVideo.Venturi
sh check-package.sh
```

The unsigned repository above is local development output. Flathub submission
and release metadata need maintainer review before distribution. A successful
build is distinct from runtime/sandbox acceptance.

## Sources and updates

Native dependency sources are pinned by archive SHA-256 or Git commit. Rust
dependencies are generated from the unchanged `Cargo.lock` by the official
`flatpak-cargo-generator.py` at commit
`9d86d336a39b9af38b86f114b6ce5820fe260f91` of
[`flatpak-builder-tools`](https://github.com/flatpak/flatpak-builder-tools).
Generate `cargo-sources.json` again whenever the lockfile changes:

```sh
python3 /path/to/flatpak-builder-tools/cargo/flatpak-cargo-generator.py \
  ../../Cargo.lock -o cargo-sources.json
```

Update the manifest's Venturi source commit along with the lockfile sources;
do not point a reviewed release at a moving branch. Build stages use frozen,
offline Cargo dependencies. Downloads occur before the build sandbox runs.

## Permissions and acceptance

Home and removable-media access lets a saved project reopen its referenced
media and user VST3 plugins. Portals alone grant the selected project file,
not all the media paths it references. Network access supports the separately
configured transcription service. The package uses Wayland or X11, PulseAudio
and DRM devices for the compositor; it does not grant arbitrary devices.

The bundled FFmpeg configuration guarantees CPU encode/decode and Rubber Band.
The runtime library search path puts `/app/lib` first so an optional runtime
codec extension cannot replace the bundled FFmpeg without Rubber Band support.
NVENC/NVDEC and Vulkan-video FFmpeg backends are not enabled by this initial
manifest; compositor rendering still uses Vulkan. Native third-party plugins,
microphones, hardware acceleration and desktop portals require runtime tests.
MCP attachment across the sandbox is not supported.

Before release, validate AppStream metadata and test project/media access,
save/reopen, import, software export, audio and the file picker inside the
installed sandbox. Test the installed package separately from an SDK build.
