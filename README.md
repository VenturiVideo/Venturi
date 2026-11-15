<div align="center">

<img src="media/icons/png/vv-icon-256.png" width="128" alt="Venturi logo">

# Venturi

**A Linux-first, performance-oriented video editor written in Rust.**

Built for editing, picture and sound, with timelines that travel to and from other NLEs via OpenTimelineIO.

[![CI](https://github.com/VenturiVideo/Venturi/actions/workflows/ci.yml/badge.svg)](https://github.com/VenturiVideo/Venturi/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/VenturiVideo/Venturi?label=release)](https://github.com/VenturiVideo/Venturi/releases/latest)
[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://rustup.rs)
[![Discord](https://img.shields.io/badge/chat-Discord-5865F2?logo=discord&logoColor=white)](https://discord.gg/3v2xjXFnZv)

[Website](https://venturivideo.github.io/Venturi/) •
[Documentation](https://kb.morrolinux.ovh/books/venturi-architecture) •
[Download](https://github.com/VenturiVideo/Venturi/releases/latest) •
[Discord](https://discord.gg/3v2xjXFnZv) •
[Features](#features) •
[Build from source](docs/BUILDING.md) •
[Architecture](ARCHITECTURE.md) •
[Contributing](#contributing)

<img src="media/venturi.png" alt="Venturi editing a multi-track timeline, with the animation curve editor and the Color window open" width="100%">

</div>

---

Venturi does little, and does it well. It is an editing-focused NLE:
multi-track cutting, the transforms and tools you actually reach for while
editing, keyframes on every parameter, transitions, compound clips, ripple
delete, and the audio tools to finish the sound in place: a mixer with an
effect chain per track and voiceover recording. No node editor, no grading
suite, no node-based compositor. Editing is where the time goes, so that is
the part that has to be perfect.

**NOTE: Venturi is alpha software and is considered unstable, especially the save format. Please use at your own risk.**


## Features

- **Fast playback and scrubbing.** Timeline-wide frame cache, optional
  background proxies, playback up to 8x with pitch-preserved audio.
- **Hardware accelerated.** Decoding on NVDEC, Vulkan video or VideoToolbox,
  compositing on the GPU (Vulkan, Metal), export on NVENC, Vulkan or
  VideoToolbox, with software as the fallback.
- **Professional timeline workflow.** Track scrubbing with audio, ripple
  delete, compound clips, copy/paste properties, copy/paste of clips and
  media between projects and Venturi windows, magnet snapping, clip speed
  from 1% to 10,000%, unlimited undo with a jumpable history.
- **A library shared by all projects.** Media and compound clips you reuse
  everywhere, and property presets (a 2x punch-in with its easing, a drop
  shadow, a voice level) applied with a drag or a shortcut, timed from the
  start or the end of any clip.
- **Audio, finished in place.** A mixer with per-track and master gain,
  balance, meters and an effect chain: 6-band equalizer and 3-band
  multiband compressor over the live spectrum, normalization (sample
  peak, true peak or EBU R128 loudness, per clip or per track), mono,
  VST3 plugins (macOS and Linux).
  Voiceover recorded straight onto the timeline.
- **Keyframes done right.** Every parameter is keyframable: crop, zoom,
  rotation, position, speed, opacity, audio gain. A dedicated curve editor
  with custom curves.
- **Titles, solid colours, filters, transitions.** Applied straight from the
  timeline, no node graph to wire up.
- **Camera Shake.** Repeatable handheld motion with keyframed strength,
  speed, pan, tilt, rotation, zoom and smoothness, plus a seed and edge
  compensation. See [the controls](docs/CAMERA_SHAKE.md).
- **Color correction.** Shadows, midtones, highlights and offset wheels,
  auto balance and GPU scopes (waveform, RGB parade, vectorscope,
  histogram), composed in 16-bit float.
- **Interoperability.** Timelines move in both directions through
  OpenTimelineIO, tested with DaVinci Resolve, for 10-bit grading and
  finishing.
- **Drivable by AI agents.** A built-in MCP server lets an agent such as
  Claude Code import, cut, measure audio, look at frames and export, on its
  own or in the window you are editing. See [docs/MCP.md](docs/MCP.md).
- **Linux first.** Developed and tested on Linux, not ported to it. Also runs
  on Apple Silicon Macs, and possibly Windows.

## Install

On Linux (x86_64 or aarch64), download and run the
[installer](https://github.com/VenturiVideo/Venturi/releases/latest/download/install.sh):

```sh
curl -fL https://github.com/VenturiVideo/Venturi/releases/latest/download/install.sh -o install.sh
bash install.sh
```

The downloaded installer stays pinned to that release; use
`bash install.sh latest` to install a newer release later.

The installer requires Bash, curl and tar. It installs the release's AppImage as
`~/.local/bin/venturi` and adds Venturi to your application menu, without sudo.
Pass a release tag such as `bash install.sh v0.9.0` to select a version
(releases before the installer existed are not supported). Set `INSTALL_DIR` to
choose another binary directory; desktop entries respect `XDG_DATA_HOME`.
Add the binary directory to your `PATH` to run `venturi` from the terminal.
To uninstall, run `bash install.sh --uninstall` with the same `INSTALL_DIR`
and `XDG_DATA_HOME` settings used for installation. This removes the binary,
menu entry, icons and MIME registration; your projects are preserved.

AppImages may require FUSE 2; without it, run
`venturi --appimage-extract-and-run`.

For macOS or a manual Linux installation, grab the latest build from the
[Releases](https://github.com/VenturiVideo/Venturi/releases/latest) page:

| Platform | File | Notes |
|---|---|---|
| Linux x86_64 / aarch64 | `Venturi-<arch>.AppImage` | FFmpeg bundled. Needs a Vulkan driver. |
| macOS (Apple Silicon) | `Venturi-arm64.dmg` | Ad-hoc signed, see [first launch on macOS](docs/BUILDING.md#opening-the-app-on-another-mac). |

```sh
chmod +x Venturi-x86_64.AppImage
./Venturi-x86_64.AppImage
```

To build from source, see [docs/BUILDING.md](docs/BUILDING.md).

## The name

The Venturi effect: when a fluid passes through a constriction, it doesn't
slow down, it speeds up. Tight constraints make Venturi faster, so your edits
stay fluid even on modest hardware.

## Contributing

Issues and pull requests are welcome. For questions, feedback and feature
ideas, join the [Discord server](https://discord.gg/3v2xjXFnZv). Before
opening a PR:

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

The `CI` workflow runs the same three steps on every pull request. Unit tests
live in `crates/<crate>/src/tests/`, not inline, see
[Where unit tests live](docs/BUILDING.md#where-unit-tests-live). The full
conventions (for humans and coding agents alike) are in [CLAUDE.md](CLAUDE.md)
and [AGENTS.md](AGENTS.md); the design is in [ARCHITECTURE.md](ARCHITECTURE.md).

## Licence

GPL-3.0-or-later, see [LICENSE](LICENSE).

DaVinci Resolve is a trademark of Blackmagic Design Pty Ltd. Venturi is an
independent project, not affiliated with or endorsed by Blackmagic Design.
