# Venturi — architecture

An editing-focused video editor: multi-track cutting,
transforms, opacity and blend modes, filters, transitions, compound clips,
solid color and text clips, clip speed, ripple/normal delete, an audio mixer
with an effect chain per track and voiceover recording. No node editor, no
color correction.

Target: Linux first, on any architecture from Apple Silicon (Asahi) to x86;
macOS too. Sources mostly H.264 (x264) 1080p, within modest RAM/VRAM.
Hardware acceleration works on x86 Linux GPUs (NVDEC/NVENC, Vulkan video)
and on macOS (VideoToolbox, Metal); on Asahi everything but compositing runs
in software today, since its Vulkan driver has no video queues.

This document describes the current state. The history of the choices is in
the git history; the rules of the pipeline refactor (invariants,
constraints) are in [plans/REFACTOR_PIPELINE.md](plans/REFACTOR_PIPELINE.md).

## Key decisions (and why)

| Area | Choice | Rationale |
|---|---|---|
| Language | Rust | mature media ecosystem, no GC, safety when editing fast with AI |
| UI | egui + eframe | immediate-mode, pure Rust, shares the wgpu device with the compositor |
| GPU | wgpu: Vulkan on Linux (Honeykrisp on Asahi), Metal on macOS | Honeykrisp is 1.3/1.4 conformant on M1/M2; one stack for UI and compositing |
| Video decode | FFmpeg (ffmpeg-next / libavcodec) with its hwaccels: VideoToolbox on macOS, NVDEC or Vulkan video on Linux; software as the fallback | frees the CPU and roughly halves the first frame after a jump ([plans/HW_DECODE.md](plans/HW_DECODE.md)). No V4L2: the Asahi AVD decoder is still unstable with multi-reference frames (practically every real x264 file). No VA-API: libva is linked at build time, so the AppImage would not start without it |
| Encode/export | FFmpeg via ffmpeg-next: libx264, or NVENC / Vulkan / VideoToolbox; each HW encoder is offered only if a test encode decodes back with the right colours; NVENC by default when it passes | encoders compiled into FFmpeg often have no hardware behind them, or write wrong colours (ANV on Gen9); no HW encoder on Asahi today |
| Audio time-stretch | libavfilter's `rubberband` filter (the system ffmpeg is already built with `--enable-librubberband`) | pitch preserved, no extra bindings to write |
| Project persistence | RON, human-readable, no format version: older files load through serde defaults and custom deserializers | debuggable, diffable with git |
| Undo/redo | command pattern (invertible commands) | light, unbounded history, consistent with a data-oriented architecture |
| Frame rate | per Timeline (not per Project): a Project holds N Timelines, each with its own fps | Project = container, Timeline = sequence with its own fps; also how OTIO files from other NLEs are organised |
| Ripple delete | global across all tracks (closes the gap everywhere, keeps A/V sync) | explicit choice for synchronised multi-track editing |
| Keyframes | on every animatable parameter: zoom, position, rotation, anchor, crop and its softness, opacity, gain, solid colour, blur radius and direction | a design requirement from the start |
| Cache | RAM frame cache with a global budget, eviction by distance from the playhead + all-intra proxies generated in the background | best perf/UX compromise on long-GOP x264 |
| Target resolution | 1080p primarily | sizes the default cache budgets |
| Waveform | yes, in the timeline | useful for cutting on speech pauses |
| Silence removal | Silero VAD (MIT, 1.3 MB, embedded) run by tract, pure Rust | tells speech from clicks and room noise, which a volume threshold cannot; no native ML runtime to ship. The model is frozen to 16 kHz by `scripts/freeze_silero_vad.py`, since tract does not type its `If` nodes |

## Data model (data-oriented, no node graph)

`vv-core/src/model.rs`. `MediaItem`, `Timeline` and `MediaFolder` live in
`IdMap`s (`vv-core/src/id_map.rs`): ids are never reused, and an undo puts an
entity back under the id it had, so commands, selections and ids given to
other programs stay valid. `Clip`s in a `Vec<Clip>` inside each `Track`, sorted by
time, with a counter-based `ClipId`. A single `Project` struct, held by the
`vv_session::Session` with its `History`, is the source of truth; workers
receive copies or snapshots.

```rust
struct Project {
    media_pool: IdMap<MediaId, MediaItem>,
    timelines: IdMap<TimelineId, Timeline>,
    folders: IdMap<FolderId, MediaFolder>,
    // + counters for ClipId, LinkGroupId and compound clips
}

struct MediaItem {
    path: PathBuf,
    meta: MediaMeta,     // duration, fps, resolution, audio (rate/channels)
    content_hash: u64,   // proxy and waveform key
    compound: Option<TimelineId>, // compound clip: a nested timeline
}

struct Timeline {
    name: String,
    fps: Rational,
    resolution: (u32, u32),
    tracks: Vec<Track>,  // order = compositing order, bottom→top
    markers: Vec<Marker>,
    master: ChannelStrip, // gain, pan, audio effect chain on the sum
}

struct Track {
    kind: TrackKind,     // Video | Audio
    clips: Vec<Clip>,    // sorted by timeline_start, non-overlapping
    muted: bool,
    solo: bool, locked: bool, armed: bool, // armed: voiceover lands here
    crossings: Vec<CrossTransition>, // transitions straddling two adjacent clips
    mix: ChannelStrip,   // audio tracks
}

struct Clip {
    id: ClipId,
    source: ClipSource,               // Media(MediaId) | SolidColor | Text | Adjustment
    source_offset: FrameIdx,          // start in the media, in Timeline frames
    timeline_start: FrameIdx,         // in Timeline frames
    timeline_len: FrameIdx,           // in Timeline frames
    effects: EffectStack,
    linked_group: Option<LinkGroupId>, // video + audio of the same import
    audio_stream_index: usize,         // which audio stream of the media
    rate: Rational,                    // Timeline frames per source frame
    speed: Rational,                   // constant clip speed, 2/1 = 200%
    pitch_correction: bool,            // audio time-stretched, not varispeed
    disabled: bool,                    // stays on the timeline, not rendered
    fade_in: FrameIdx, fade_out: FrameIdx,
}

struct EffectStack {
    transform: TransformTracks,  // one Keyframed<f32> per parameter (opacity too) + flip
    gain_db: Keyframed<f32>,
    color: Option<Keyframed<Rgba>>,   // SolidColor and Text
    title: Option<TitleParams>,       // Text only
    filters: Vec<ClipFilter>,         // grayscale, exposure, box/gaussian blur, in order
    transition_in: Option<Transition>, transition_out: Option<Transition>,
    blend_mode: BlendMode,            // 15 separable modes
    masks: Vec<ClipMask>,             // rectangle/ellipse/path, combined in order
}

struct Keyframed<T> { keyframes: Vec<(FrameIdx, T, Interpolation)>, default: T }
// Interpolation: Hold, Linear, EaseInOut, EaseIn, EaseOut, Bezier { c1, c2 }
```

- Importing a media: one video clip plus one audio clip per audio stream,
  all in the same `linked_group`. Selection, drag and delete treat the group
  as a unit.
- Transforms are evaluated at runtime (`Keyframed::value_at`, CPU) and go to
  the shader as uniforms: **never baked into cached frames**, so editing a
  parameter does not invalidate the buffer.
- A clip whose media has an fps different from the Timeline's is
  **conformed**: `Clip::rate` (= Timeline fps / media fps, computed on
  insertion and recomputed when a project is loaded) is the only place where
  the two frame spaces meet. `source_frame_at()`/`timeline_frame_at()` apply
  it, so a clip lasts its real time on the Timeline and the video repeats or
  skips a frame when needed (59.94 on 60: one duplicated every ~17 s)
  instead of drifting out of sync with the audio, which always plays at real
  speed.
- **Clip speed** folds into the same mapping: `rate = conform / speed`, so
  every consumer of `rate` (preview, export, trims, `render_ahead`) follows
  it with no special case. `SetClipSpeed` keeps `source_in` and ripples by
  the change of length. Audio: varispeed in `mix_range` (interpolated read),
  or with `pitch_correction` the clip's source range stretched with
  `rubberband` (background worker in the preview, silent until ready;
  synchronous on export). See plans/CLIP_SPEED.md.
- A clip's range is `source_offset` + `timeline_len`, both in Timeline
  frames (like OTIO's `source_range`); `source_in()`/`source_out()` are
  derived from them. Splits and trims land exactly on the chosen position
  even in the middle of a source frame, without losing the content's phase.

## Decode pipeline + cache

- **Decode** (`vv-media/src/decode.rs`): `Decoder` with `next_frame` and
  `seek_to_time` (keyframe ≤ target, then sequential decode). Every pixel
  format is normalised to 8-bit 4:2:0 (`FrameYuv420`), with the chroma
  planar or, for NV12 sources and HW frames, interleaved (read as `.rg` by
  the shader, no sws); the conversion to RGB happens in the shader.
- **HW decode** (`vv-media/src/hw.rs`, `Decoder::open_with`): one path over
  FFmpeg's hwaccels for every backend (`HwDevice`: VideoToolbox, CUDA,
  Vulkan on a named GPU), one device per backend per process. Frames are
  downloaded to system memory (mapped on VideoToolbox) and go into the
  same cache as software ones. FFmpeg falls back to software silently, so
  HW is confirmed by the format of the frames: a media whose hwaccel does
  not deliver, or errors mid-stream, reopens in software at the same
  position and stays there (`hw::FAILED`). Each HW decoder leases its
  surfaces from a global budget (Settings > Playback, default from the
  physical RAM); over budget it opens in software, and the decoders behind
  the playhead and the proxies (`HwPriority::Low`) may take only half.
  Which GPU is the setting's (`vv-app/src/hw_decode.rs`): `Auto` is
  VideoToolbox on macOS; on Linux Vulkan on the integrated GPU of a hybrid
  laptop (NVDEC would wake the discrete one), else NVDEC if there is an
  NVIDIA GPU, then Vulkan. Thumbnails and probing stay in software (device
  setup would dominate).
- **Timeline buffer** (`vv-app/src/render_ahead.rs`, `RenderAhead`): a
  thread walks the timeline forward from the playhead (`lookahead_secs`,
  default 3s; behind it keeps `behind_secs`, default 2s, with the budget
  that is left), crossing cuts, gaps and tracks without special cases, and
  fills a `SharedFrameCache` (`vv-media/src/cache.rs`) keyed by
  `(MediaId, source frame)` with a global byte budget (default 1.2 GB,
  configurable). Eviction is a single pass (`reconcile`): out of the window
  goes first, then the frames farthest from the playhead. The threshold
  beyond which a real seek beats decoding forward adapts to the observed
  GOP. Lookahead and behind are configurable in Settings > Playback; at
  speeds > 1x the lookahead scales.
- **Media pool preview** (`browsing_media`): a second `RenderAhead` on a
  synthetic timeline holding only that media, seeking from the bar under the
  viewer. Audio goes through the same `TimelineAudio` as the timeline
  (`sync_media`: a snapshot with all the media's streams), whose clock acts
  as the playhead.
- **Proxy** (`vv-media/src/proxy.rs`, thread `vv-app::proxy_worker`): for
  every imported media, all-intra H.264 in
  `$XDG_CACHE_HOME/venturi/proxies/`, keyed by `content_hash` (fast
  fingerprint: canonical path + size + mtime) + quality (low 640px, medium
  960px, high 1920px; one file per quality). Toggle in the Playback menu and
  in Settings > Playback (quality), off by default; export always uses the
  original sources.
- **Waveform** (`vv-media/src/waveform.rs`, thread
  `vv-app::waveform_worker`): peaks per `(content_hash, stream)` computed in
  streaming and saved to disk; the timeline loads them in memory and draws
  them on the audio clips.

## GPU compositing (wgpu, for each output frame)

1. A frame's layers are the clips active on each video track, bottom to top
   (`Timeline::active_video_clips_at`); each one's source frame comes from
   `Clip::source_frame_at`, obtained through `FrameProvider`
   (`vv-session/src/frame_provider.rs`): from the `RenderAhead` cache in
   preview, streamed in export.
2. Upload of the YUV planes, conversion to RGB in the shader
   (`vv-render/src/shaders/transform.wgsl`, BT.601/709/2020 matrix and range
   from the source).
3. The shader fits the source into the output frame keeping its aspect
   ratio — bars (letterbox/pillarbox) instead of stretching, e.g. a 9:16
   clip on a 16:9 timeline — and applies the `Transform` to it reasoning in
   *output* coordinates, with the Y axis pointing up as in an NLE (the
   shader flips it, uvs point down) and parameters in pixels — of the
   timeline for position and anchor, of the media (native resolution, not
   the proxy's) for crop and softness, converted to fractions in the uniform
   (`OutputFrame`, `LayerContent::Video::source_size`): `zoom` (per axis) and
   `rotation` act around the `anchor`, `position` moves the clip in the
   frame, `flip` mirrors it, `crop` cuts each side by a fraction (0 =
   untouched) — with `crop_softness` feathering the edge through alpha,
   inwards if negative and outwards if positive — without recentring or
   resizing the rest.
4. One pass per layer on the same texture, alpha-over
   (`Compositor::render_layers`): the bars of the upper layer come out with
   alpha 0 and let the one below show. SolidColor and Text are layers like
   any other: the shader uses the layer colour (for text, with the glyph
   coverage rasterised by `vv-render/src/text.rs` in the Y plane). An
   Adjustment layer copies the stack composed so far and redraws it with its
   own transform/filters, replacing it (clear colour where uncovered, which
   keeps OTIO round trips rendering the same) and mixing by opacity (`LayerContent::Adjustment`).
   Masks (`vv-core/src/mask.rs`) live in the layer's own space (timeline
   pixels from the clip's centre at zoom 1, Y up), so the transform carries
   them; the shader evaluates them per pixel as signed distance fields from
   a second uniform (paths arrive already flattened to polygons). They
   scale the layer's coverage, or for an adjustment layer its final mix with
   the stack below.
5. Preview: composes at the resolution of the decoded frame widened to the
   timeline aspect (`vv_render::fit_output_size`), so the bars already show
   while editing without upscaling the content;
   `Compositor::render_layers_to_texture` stays on the GPU and the texture
   is registered in `egui-wgpu`, with no readback. Export:
   `Compositor::render_layers_i420` composes at the timeline resolution,
   converts to BT.709 I420 on the GPU (compute shader) and reads back the
   planes for the encoder. Decode and composition run on two pipelined
   threads, encode on the export thread itself, the audio mix on a fourth.
6. No video clips (gap on every track): a black frame, the composition of an
   empty stack.

Each layer carries its `opacity` and `BlendMode`. `Normal` uses the
pipeline's alpha blending; the other 14 modes copy the stack composed so far
(the backdrop) and mix with it in the shader (`blend_channel` in
`transform.wgsl`).

## Audio pipeline

- Block decode (ffmpeg, `vv_media::decode_audio_streams_streaming`) with
  swresample resampling to 48 kHz → conversion to the mix channels
  (`vv_audio::remix_channels_into`) → mix of the non-muted audio tracks with
  keyframed gain evaluated in 800-sample blocks (`mix_range`) → `cpal`
  output.
- Preview and export use the same `mix_range`: export mixes to 2 channels,
  preview to the device's native channels (asking for others makes PipeWire
  insert a remix that adds latency).
- **Preview** (`vv-app/src/timeline_audio.rs`, `TimelineAudio`): a single
  `cpal` stream opened at startup and never reopened (`vv_audio::Mixer`).
  The UI thread builds an immutable snapshot of the clips (`MixSnapshot`,
  with `Arc`s to the already converted buffers) on every change of
  `History::generation` or when a buffer arrives, and publishes it to the
  callback with a `try_lock`; old snapshots are freed on the UI thread.
  Buffers are decoded in the background per `(path, audio_stream_index)`
  (`mix_buffers.rs`) and published partially as they grow: the start of the
  track plays right away, silence only past the part already decoded.
- **Channel strips**: every audio track and the timeline master have a
  `ChannelStrip` (gain, pan, an insert chain before the fader): 6-band
  equalizer, 3-band multiband compressor, mono and normalization. The mix is
  per track, then the master strip on the sum.
- **Normalization**: brings a channel to a target measured as sample peak,
  true peak (BS.1770) or integrated loudness (EBU R128); per clip
  (`SetLevel::Independent`) or one gain for all the clips of the track
  (`Relative`). The levels are measured off the audio thread by the level
  worker in `mix_buffers.rs` (`LevelAnalysis`); until a level is ready the
  last one keeps playing.
- **Clip speed with pitch correction**: the clip's source range is stretched
  with `rubberband` by a third `mix_buffers` worker, silent until ready;
  synchronous on export.
- **Clock**: the mixer position, in timeline samples, is the playhead
  (`drive_playback`); the video chases it. A gap is just silence, the clock
  keeps advancing. With no audio device the clock is wall-clock.
- **Fast forward** ("a" key, 2x/4x/8x): 8s windows of the mix rendered and
  stretched with `rubberband` in the background, queued to the mixer without
  reopening the stream (`StretchedWindow`); the speed takes effect when the
  first window is ready.
- **Scrub**: an 80ms fragment of the mix at the new position (option in the
  Timeline menu).

## Undo/redo

`trait Command { fn apply(&mut self, p: &mut Project); fn undo(&self, p: &mut
Project); }` in `vv-core/src/command.rs`. Every command captures the
"before" state when it runs; `History` keeps the undo/redo stacks and a
`generation` that changes on every edit (workers compare it to know when to
resync). Several commands in a single step: `CompositeCommand`. Commands:
`InsertClip`, `LiftDelete`, `RippleDeleteGap`, `MoveClips`, `TrimClip`, `SlipClip`,
`SplitClip`, `LinkClips`, `UnlinkClip`, `AddTrack`, `RemoveTrack`,
`SetClipColor`, `UpsertKeyframe`, `RemoveKeyframe`, `RemoveMedia`,
`RelinkMedia` and more; a clip's static values (transform parameters, flip,
gain, title) go through `SetClipValue` (`set_clip_*`). `insert_overwriting`
clears the space under the inserted clips (paste, duplicate, drop from the
media pool). `vv-core/src/edit.rs` builds the multi-step editing operations
(split, delete, ripple, `delete_ranges`, insert) on explicit targets; the UI
maps selection and playhead onto them and owns the undo grouping. Every
project change, from the UI, MCP or a session job, is a `Command` run
through the `Session`'s `History`, so undo and the unsaved flag see all of
them.

## Threading

- **UI**: the egui loop, command dispatch, preview compositing. It owns the
  `Session`, or with MCP on checks it out of a shared slot for the length of
  each frame (see MCP server).
- **Audio**: the `Mixer`'s `cpal` callback, mixes straight from the current
  snapshot, with no allocations or blocking locks (`try_lock` only).
- **`RenderAhead`**: one decode thread per buffer: the timeline, the media
  pool preview and, while slipping, each of the two ends.
- **`mix_buffers`**: three serial workers: decode and resample of the
  mixer's audio buffers, pitch-corrected clip-speed stretch, normalization
  levels.
- **Fast forward stretch**: one thread per window, at most one request in
  flight.
- **`proxy_worker`**, **`waveform_worker`**, **`thumbnail_worker`**: serial
  queues, one thread each.
- **Session jobs** (`vv-session/src/jobs.rs`): import probing on a pool of
  one thread per core; OTIO import, relink and forced relink on a thread
  each.
- **Export**: a job thread on a clone of `Project`, with shared progress and
  cancellation. It spawns scoped decode, compose and audio mix threads and
  encodes itself.
- **Silence analysis**: one thread for the media the dialog has not
  measured yet.
- **MCP**: the transport (stdio, or the socket listener on a tokio
  runtime), the editor's host thread, a worker for slow tools.
- **Short-lived**: file dialogs, HW decoder and encoder probes, text
  warm-up, Wayland drag and drop.

## MCP server

`vv-mcp` turns MCP tool calls into `ToolCall`s and runs them with
`dispatch` on a `vv_session::Session`; edits are the same commands the UI
runs. The transport (rmcp, on a tokio thread) only submits calls through a
channel (`McpHandle` → `McpInbox`); whoever owns the `Session` answers them.
Headless (`vv-app mcp`) that is `run_headless`. In the editor window it is a
host thread of its own (`vv-app/src/mcp_host.rs`): between two frames the
`Session` sits in a shared slot, and the UI checks it out for the length of
each frame, so calls are answered even when the window is not drawn. The
host holds project-changing calls while the user is mid-gesture (so they
never join the user's undo group) and refuses them while a dialog waits.
Slow calls (`render_frame`, `get_audio_levels`) run on a worker thread and
are polled. The editor-only tools (`get_state`, `screenshot_ui`,
`set_active_timeline`) return an error when headless. The window serves on a
0600 Unix socket; `vv-app mcp --attach` bridges a client's stdio to it.
Usage: [docs/MCP.md](docs/MCP.md).

## Cargo workspace layout

```
venturi/
  crates/
    vv-core/     # data model, commands, undo/redo, RON persistence
    vv-media/    # probe, decode, frame cache, proxy, waveform, encode
    vv-render/   # wgpu compositor, wgsl shaders, text
    vv-audio/    # mixer, resample, time-stretch, cpal output
    vv-vad/      # voice activity detection (Silero VAD on tract)
    vv-session/  # UI-free application core: export, import/relink workers
    vv-mcp/      # MCP server: tools over a Session, stdio and socket transports
    vv-app/      # egui UI (timeline, viewer, panels), playback, MCP editor host
```

## Feature status

Done:
- Import (multi-stream audio too), media pool with preview, drag onto the
  timeline (several selected items are queued in panel order),
  multi-selection (click, ctrl, shift, rectangle) and deletion
  (Del/Backspace) of items: clips using a deleted media stay on the timeline
  in red and the viewer shows "Media offline" (`vv_core::RemoveMedia`).
- Multi-track timeline: add track, drag, trim, split at the playhead (T),
  normal delete (Del/Backspace), ripple delete ("<" key), copy/paste,
  link/unlink, multi-selection (click, ctrl, shift, rectangle), "selection
  follows playhead", snapping, zoom (Ctrl+/Ctrl-).
- Slip tool (toolbar under the viewer, exclusive with the selection arrow):
  dragging a clip slides its media (linked group included) without moving or
  resizing it; the viewer splits into its first and last frame, two
  single-media `RenderAhead`s (`vv-app/src/slip_viewer.rs`), and the timeline
  outlines the media available on each side.
- Transport bar under the viewer (`vv-app/src/transport.rs`): playhead,
  in/out markers (I/O keys), play/pause. On the media pool preview, in/out
  delimit the portion dragged from the viewer to the timeline; on the
  timeline they delimit the export (not saved in the project).
- Playback: Space play/pause, "a" fast forward, arrows frame by frame (held
  down they run at 0.5x), audio of all tracks, audio scrub, audio meter.
- Effects: zoom X/Y (linkable), position, rotation, anchor point, flip, crop
  of the four sides with softness (inwards or outwards), all in pixels, gain,
  SolidColor colour; static or keyframed (Hold, Linear, ease in/out, free
  Bezier curve in the keyframe editor) from the
  properties panel, split into the Video tab (Transform and Cropping
  sections, reset per parameter and per section) and the Audio tab (gain).
  Every transform parameter has its own keyframes (`TransformTracks`): the
  row's diamond is red when the playhead is on a keyframe, and otherwise
  carries the arrows to jump to the nearest keyframe in that direction. The
  values shown are those of the first selected clip on that track kind;
  every change goes to all the others as a single command
  (`CompositeCommand`, so a single undo).
- Clips with an fps different from the timeline's conformed on insertion
  (`Clip::rate`), in preview and in export.
- Text clips (`ClipSource::Text`): font, style, colour, alignment, shadow
  and background from the properties panel.
- Opacity and 15 blend modes per clip; filters (grayscale, exposure, box
  and gaussian blur with keyframable radius and direction); masks
  (rectangle, ellipse, bezier path; feather, invert, add/subtract/intersect;
  handles and pen in the viewer), which on an adjustment clip limit where its
  filters apply; Push transitions on a clip's
  edges or across a cut; fades; adjustment clips; compound clips (a nested
  timeline in the media pool); markers.
- Mixer: per-track and master gain, pan, solo, equalizer, multiband
  compressor, mono and normalization (sample peak, true peak, loudness);
  voiceover recording onto an armed track.
- Silence removal driven by voice activity detection (`vv-vad`).
- Proxies and waveforms in the background.
- Hardware video decoding for playback, proxies and export, with software
  fallback per media.
- Audio-only media (wav, mp3, flac…): nominal fps `AUDIO_ONLY_FPS`, no proxy
  or thumbnail, only audio clips on the timeline.
- H.264 + AAC export in MP4 (Ctrl+Shift+E) from a settings window:
  destination, in/out range or the whole timeline, decoder (CPU or any GPU
  decoder of the machine, the one of Settings > Playback first), video encoder (x264, NVENC or Vulkan; x264 or
  VideoToolbox on macOS) with preset and quality, reduced resolution, audio encoder
  (native AAC/FDK) with preset and bitrate. Default: NVENC and FDK if
  available, otherwise x264 `superfast` CRF 20 and native AAC. The last
  settings persist for the session.
- Project as a `.vvproj` file in RON (Ctrl+O, Ctrl+S, Ctrl+Shift+S).
- OpenTimelineIO (`vv-core/src/otio/`, File → Export/Import OTIO).
  Export: `source_range` at the timeline fps, holes as `Gap`, effects,
  linked groups and audio streams in `metadata.venturi`. Import: opens the
  file as a new project quantising times to the timeline frame; our own file
  comes back identical, from other editors video and audio of the same span
  are relinked, and whatever cannot be represented (transitions, effects,
  non-file references) is reported.

Not yet:
- Speed ramps (keyframed speed), reverse and freeze frame.

## Environment setup

Dependencies, build and tests: see [docs/BUILDING.md](docs/BUILDING.md).
