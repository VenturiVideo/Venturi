# Driving Venturi from an AI agent (MCP)

Venturi speaks the [Model Context Protocol](https://modelcontextprotocol.io):
an agent such as Claude Code can import media, build and cut timelines, add
titles, look at frames, measure audio and export, through the same editing
code the UI uses. It works in two ways:

- **Headless**: `vv-app mcp` runs without a window. The agent has a project
  of its own.
- **Attached**: the agent works in the Venturi window you have open, on the
  project you are editing. You see every change, and you can undo it.

Both speak MCP over stdin/stdout, so any MCP client can use them.

## Setup

The commands below use `vv-app`, the binary built from source. With a
release, use the AppImage itself (`/path/to/Venturi-x86_64.AppImage mcp`) or,
on macOS, `/Applications/Venturi.app/Contents/MacOS/vv-app mcp`.

### Headless

```sh
claude mcp add venturi -- /path/to/vv-app mcp
```

To start from an existing project, add its path:
`vv-app mcp /path/to/project.vvproj`. The agent saves with `save_project`.
Nothing is saved automatically.

In other MCP clients, the server entry looks like this:

```json
{ "mcpServers": { "venturi": { "command": "/path/to/vv-app", "args": ["mcp"] } } }
```

### Attached to the editor window

1. In Venturi, turn on **Settings > Integrations > Let AI agents work in this
   window (MCP)**. It stays on across restarts. To turn it on for one run
   only, start Venturi with `--mcp`. The Integrations section shows the
   exact command to use.
2. Register the command:

   ```sh
   claude mcp add venturi -- /path/to/vv-app mcp --attach
   ```

`--attach` connects to the Venturi window that is running. With several
windows open, pick one with `--pid <process id>`; the error message lists the
candidates. While a client is connected, the toolbar shows an orange
**● MCP**. Its tooltip shows the last tool the agent called.

The window listens on a Unix socket,
`$XDG_RUNTIME_DIR/venturi/mcp-<pid>.sock` (in the temporary folder when
`XDG_RUNTIME_DIR` is not set), that only your user can open.
Venturi opens no network port. Windows and the Flatpak build do not support
attaching yet.

While attached, the agent shares the window with you:

- It keeps working while the window is hidden or on another workspace;
  the window catches up when you come back. Only `screenshot_ui` needs the
  window to be drawn.
- Its edits wait while you are in the middle of a gesture (dragging a clip,
  a slider, a handle in the viewer). They never merge into your undo step:
  each agent call is a step of its own in **Edit > Undo History**.
- While a dialog is waiting for your answer (unsaved changes, reusing OTIO
  media, forced relink), the agent's edits are refused. Reading still works.
- `new_project` and `open_project` are refused if the open project has
  unsaved changes.
- Media the agent imports do not change your selection or the viewer. A
  timeline it imports is not opened. Its exports appear in the usual
  progress window.

## Conventions

- **Ids are strings**: media, timelines, clips, markers, export jobs. Take
  them from the results; don't build them yourself.
- **Frames are integers.** Positions and lengths are frames of the timeline
  (its fps is in `get_timeline`). Only `source_in`/`source_out` and
  `get_audio_levels` on a media are frames of the media. Ranges are
  `[start, end)`: the end is excluded.
- **Tracks are named** as in the UI: `V1`, `V2`… and `A1`, `A2`….
- **Colors** of clips and markers are the editor's palette: `red`, `orange`,
  `yellow`, `green`, `cyan`, `blue`, `indigo`, `purple`, `magenta`, `rose`,
  `slate`, `gray`.
- **One call, one undo step.** `undo` reverts the last step, whoever made it.
  A call that fails leaves nothing behind.
- **Revisions.** Every result that describes a timeline carries its
  `revision`, and every edit returns the new one. Pass it back as
  `if_revision`: if the timeline changed in the meantime (in attached mode
  you may be editing it too), the edit is refused instead of landing on
  different material. Edits to other timelines do not change it.
- **Locked tracks** are never changed; a call that would change one fails.
- Failures come back as tool errors with a plain message, e.g.
  `track V1 is locked` or `no clip "12" in this timeline`.

## Tools

### Reading

| Tool | What it returns |
|---|---|
| `get_project` | Media pool (id, kind, fps, duration in frames and seconds, resolution, audio streams, offline flag), timelines, folders, file path, unsaved flag |
| `get_timeline(timeline_id)` | Tracks with their clips (start, end, source media and in/out, link group, fades, effects set) and markers |
| `get_clip(timeline_id, clip_id)` | One clip with all its effect values, keyframes included |
| `get_markers(timeline_id)` | The markers: id, start, duration, note, color |

### Project

| Tool | Notes |
|---|---|
| `new_project`, `open_project(path)` | Replace the open project |
| `save_project(path?)` | Without `path`, saves to the current file |
| `import_media(paths)` | Answers once every file is probed, with ids and metadata; files already in the pool are reported with their existing ids |
| `import_otio(path, reuse_existing_media?)` | Timelines from an OpenTimelineIO file (e.g. from DaVinci Resolve) |
| `create_timeline(name, from_media?, fps?, resolution?)` | Tracks V1 and A1. Format taken from `from_media`, or given; default 25 fps, 1920×1080 |
| `add_track(timeline_id, kind)` | `video` or `audio`; returns the new track's name |
| `set_track(timeline_id, track, muted?, solo?, locked?)` | |

### Editing

Every call below is one undo step.

| Tool | Notes |
|---|---|
| `insert_clip(timeline_id, media_id, at, source_in?, source_out?, video_track?, audio_track?, video?, audio?)` | Overwrites what is at `at`. Puts the video on the timeline plus one linked audio clip per audio stream; audio tracks are created as needed |
| `split(timeline_id, frame, clip_ids?)` | Returns the left and right id of each cut clip |
| `delete_clips(timeline_id, clip_ids, ripple?)` | With `ripple`, the gaps close and linked clips go too |
| `delete_ranges(timeline_id, ranges, ripple?, tracks?, media_id?)` | Removes `[start, end)` ranges, cutting clips at the edges. With `ripple`, every unlocked track closes up, so audio and video stay in sync. With `media_id` the ranges are frames of that media: Venturi finds that material wherever it is on the timeline, after any earlier cut, and reports the timeline ranges it removed |
| `move_clips(timeline_id, moves)` | `moves`: `{clip_id, start, track?}`. Overwrites what is at the destination; linked clips must be listed too |
| `trim_clip(timeline_id, clip_id, edge, frame)` | `edge` is `start` or `end`; the error names the allowed range |
| `set_clip_properties(timeline_id, clip_ids, …)` | Static values: `opacity` (0-100), `position` and `scale` (`[x, y]`), `rotation` (degrees), `gain_db`, `disabled`, `fade_in`/`fade_out` (frames), `fill_color` (solid color clips). Values with keyframes keep following them; a warning says so |
| `set_transition(timeline_id, clip_ids, edge, kind?, duration?, direction?, ease?, curve?)` | Video clips. On the `start` or `end` edge, `push` (default) slides the clip in from, or out to, the edge of the frame over what is below; `kind: none` removes it. Defaults: 0.45 s, `right`, `in_out`, curve 0.5; options not given keep the value already on that edge. Reported by `get_clip` as `transition_in`/`transition_out` |
| `add_title(timeline_id, text, at, duration?, track?, size?, color?, position?)` | |
| `add_solid_color(timeline_id, at, duration?, track?, color?)` | |
| `add_adjustment_clip(timeline_id, at, duration?, track?)` | |
| `link_clips`, `unlink_clips(timeline_id, clip_ids)` | Unlinking dissolves the whole group |
| `set_clip_color(timeline_id, clip_ids, color)` | The clips' color on the timeline, to tag them (e.g. takes to review): a palette color or `none` for the default. Reported as `clip_color` |
| `set_clip_masks(timeline_id, clip_id, masks)` | Video clips. Replaces the masks: `rectangle`, `ellipse` or `path` (`points`, at least 3), each with `center`, `size`, `rotation`, `roundness`, `feather`, `expansion`, `opacity` (0-100), `invert` and `mode` (`add`, `subtract`, `intersect`). Pixels of the timeline from the clip's center, Y up. On an adjustment clip they limit where its filters apply. Reported by `get_clip` under `effects.masks` |
| `add_marker(timeline_id, at, duration?, note?, color?)`, `edit_marker`, `delete_marker` | Marker color from the palette, default yellow |
| `undo`, `redo` | Return the name of the step |

### Seeing and hearing

| Tool | Notes |
|---|---|
| `render_frame(timeline_id, frame, max_width?)` | A PNG of one frame (default 960 px wide). The frame is decoded exactly as the export decodes it, never taken from the preview cache |
| `get_audio_levels(media_id + stream? \| timeline_id, start, end, window?)` | RMS and peak in dBFS per window of `window` frames (default 1). Digital silence reads -120. Measures one audio stream of a media, or everything the timeline plays mixed together. At most 20 000 windows per call |

### Export

| Tool | Notes |
|---|---|
| `export(timeline_id, path, range?, scale_percent?, audio?)` | Starts in the background and returns a `job_id`. Edits made afterwards do not affect it. One export at a time |
| `export_status(job_id)` | `running`, `done`, `failed` or `cancelled`, with frames written of the total, overall `fps` and the `stage_fps` of decode, compose and encode (each stage's own speed, without waiting on the others) |
| `cancel_export(job_id)` | |

### Editor window only

Without `--attach` these return an error asking to attach to the window.

| Tool | Notes |
|---|---|
| `get_state` | The open timeline, the playhead and your selection, so you can tell the agent "cut *this*" |
| `screenshot_ui` | A PNG of the whole window |
| `set_active_timeline(timeline_id)` | Shows that timeline in the editor |

## Example: removing the pauses from a recording

What an agent does, step by step, to cut every silence longer than half a
second from `talk.mp4` (25 fps):

1. `import_media({"paths": ["/home/me/talk.mp4"]})` returns the media id,
   its fps and its duration.
2. `create_timeline({"name": "Talk", "from_media": "<media id>"})`, then
   `insert_clip({"timeline_id": "<id>", "media_id": "<media id>", "at": 0})`.
   The measurements below are in frames of the media; `delete_ranges` with
   `media_id` takes them as they are.
3. `get_audio_levels({"media_id": "<media id>", "start": 0, "end": 150})`
   returns one RMS value per frame:

   ```json
   { "window_frames": 1, "rms_db": [-21.3, -21.4, …, -120.0, -120.0, …, -21.2], … }
   ```

4. The agent decides what a pause is, for example RMS below -50 dB for at
   least 12 frames. It keeps a few frames at each edge so words are not
   clipped, and turns what is left into ranges, e.g. `[[27, 60], [102, 123]]`.
5. `delete_ranges({"timeline_id": "<id>", "media_id": "<media id>",
   "ranges": [[27, 60], [102, 123]], "ripple": true, "if_revision": "<revision>"})`
   removes both in one undo step, with audio and video still in sync. With
   `media_id` the ranges stay valid even if the timeline was already cut
   before, and `if_revision` refuses the edit if the timeline changed since
   it was read.
6. `get_audio_levels` on the timeline, and `render_frame` around the cuts,
   confirm the result. `undo` restores everything if it went wrong.
7. `export({"timeline_id": "<id>", "path": "/home/me/talk_cut.mp4"})`, then
   `export_status` until the state is `done`.

When the pauses depend on the words, e.g. repeated takes, the agent
transcribes the audio and cuts by timestamps the same way (see below).

## Transcribing

Venturi does not transcribe yet, so an agent that needs the words (repeated
takes, cutting by script) runs a speech-to-text tool of its own, such as
whisper.cpp or faster-whisper. To keep that fast and cheap:

- **Install it once**, not in a throwaway environment for every session.
  The repository has one ready in `container/whisper/` (faster-whisper,
  word timestamps), built once and reused:

  ```sh
  mkdir -p ~/.cache/whisper-models   # model cache, downloaded on first use
  cd container/whisper
  MEDIA_DIR=/path/to/media podman compose run --rm whisper piece.wav --language it
  ```

  With an NVIDIA GPU and the NVIDIA Container Toolkit (CDI), run the
  `whisper-gpu` service instead of `whisper`: same arguments, much
  faster (under a minute for 3 minutes of audio with `crisper`).

  The file path is relative to `MEDIA_DIR`; `--stream N` picks an audio
  stream of a video, `--model` the model (default `crisper`, below). It
  writes `piece.wav.transcript.json` next to the input:
  `segments` with `start`/`end`/`text` in seconds and `words` as
  `{s, e, w}`, plus `pauses` (`{s, e}`, stretches quieter than `--pause-db`,
  default −45 dBFS, lasting at least `--min-pause`, default 0.3 s). The
  audio is transcribed in independent pieces of about `--chunk` seconds
  (default 10), split at those pauses: decoding long windows, each
  conditioned on the text before, merges retakes into one sentence, loops
  on a word or drops whole passages. A whisper.cpp or
  `pipx install faster-whisper` already on the system works as well, with
  those problems.
- **`crisper` finds repeated takes.**
  [CrisperWhisper](https://huggingface.co/nyrahealth/faster_CrisperWhisper)
  (about 3 GB) transcribes verbatim, keeping false starts and retakes; plain
  Whisper writes clean text and merges them into one fluent sentence
  (stretching a word over a second or more), so they vanish from the
  transcript. Trained on English and German, `crisper` spells other
  languages badly (glued words, `blocked_out_string…` tokens) and its word
  times drift by a few tenths of a second: use it to find which takes
  repeat, keep the last complete one, and put each cut edge inside one of
  the `pauses` rather than on a word time. When readable text matters more
  than every retake (cutting by a script in another language),
  `--model medium` (about 1.5 GB) spells it properly.
- **Don't analyse audio on the host**: its Python may lack numpy. The
  transcript's `pauses` and `get_audio_levels` cover the cut edges; any
  other script runs in the container, where numpy is installed and
  `MEDIA_DIR` is `/work`:
  `podman compose run --rm --entrypoint python whisper script.py`.
- **Transcribe only what the timeline uses.** A timeline often takes a few
  seconds out of a long recording. `get_timeline` gives each clip's media
  and `source_in`/`source_out` (media frames): merge the ranges per media,
  widen each by about 2 seconds on both sides (clamped to the media) so the
  words at the edges are not cut, and extract just those pieces into
  `MEDIA_DIR`, e.g.
  `ffmpeg -ss <start s> -to <end s> -i media.mp4 -map 0:a:<stream> -ac 1 -ar 16000 piece.wav`
  (seconds = frame ÷ the media's fps, from `get_project`). Transcribe the
  pieces, not the whole file.
- **Keep the times in the media's frames**: add each piece's start back to
  the word times, then frame = seconds × media fps. Cut with
  `delete_ranges` and `media_id`, which takes exactly those frames and finds
  them on the timeline, even after earlier cuts.
- **Keep the transcript** of a media for the rest of the session instead of
  transcribing it again after every edit: the media's frames do not change
  when the timeline does.

## Screenshots of web pages

For cover images (a product's homepage over the talk), take the screenshot
with `ghcr.io/karakeep-app/karakeep-chrome`, a headless Chromium driven
over the DevTools protocol (CDP) on port 9222:

```sh
podman run -d --name vv-shot -p 127.0.0.1:9222:9222 ghcr.io/karakeep-app/karakeep-chrome:latest
node shot.mjs out/          # the script below, Node 22+ (built-in WebSocket)
podman rm -f vv-shot
```

- Its `headless-shell` ignores `--screenshot` on the command line: drive it
  through CDP. `docker.io/zenika/alpine-chrome --screenshot` renders some
  sites without their CSS and hangs on others.
- The `webSocketDebuggerUrl` it returns names the container's address:
  replace the host with `127.0.0.1:9222`.
- Settings that give a clean frame: viewport 1920×1080 at scale factor 1
  (`Emulation.setDeviceMetricsOverride`), wait for `Page.loadEventFired`
  plus about 4 s for animations, then click the cookie banner's refusing
  or accepting button and hide the scrollbars, wait 1.5 s and capture.

```js
import { writeFileSync } from "node:fs";
const sites = { netbird: "https://netbird.io" };
const sleep = ms => new Promise(r => setTimeout(r, ms));
for (const [name, url] of Object.entries(sites)) {
  const t = await (await fetch("http://127.0.0.1:9222/json/new?about:blank", { method: "PUT" })).json();
  const ws = new WebSocket(t.webSocketDebuggerUrl.replace(/ws:\/\/[^/]+/, "ws://127.0.0.1:9222"));
  await new Promise(r => (ws.onopen = r));
  let id = 0; const pending = new Map(); let loaded = false;
  ws.onmessage = m => { const d = JSON.parse(m.data);
    if (pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); }
    else if (d.method === "Page.loadEventFired") loaded = true; };
  const send = (method, params = {}) => new Promise(r => { pending.set(++id, r); ws.send(JSON.stringify({ id, method, params })); });
  await send("Page.enable");
  await send("Emulation.setDeviceMetricsOverride", { width: 1920, height: 1080, deviceScaleFactor: 1, mobile: false });
  await send("Page.navigate", { url });
  for (let i = 0; i < 150 && !loaded; i++) await sleep(100);
  await sleep(4000);
  await send("Runtime.evaluate", { expression: `(() => {
    const st = document.createElement('style');
    st.textContent = '::-webkit-scrollbar{display:none}html{scrollbar-width:none}';
    document.head.appendChild(st);
    const re = /^(reject all|required only cookies|accept( all)?( cookies)?|i agree|got it)$/i;
    for (const b of document.querySelectorAll('button, a[role=button]'))
      if (re.test(b.innerText.trim())) { b.click(); return; } })()` });
  await sleep(1500);
  const r = await send("Page.captureScreenshot", { format: "png" });
  writeFileSync(`${process.argv[2]}/${name}.png`, Buffer.from(r.result.data, "base64"));
  ws.close();
  await fetch(`http://127.0.0.1:9222/json/close/${t.id}`);
}
```

Look at every image before using it: a banner the regex misses stays in
the frame. Put the images on a video track above the talk with
`insert_clip`; an image media has its own fps (25), so `source_out` is in
those frames, not the timeline's.

## Limits

- `get_audio_levels` on a timeline decodes the whole audio files involved,
  so it is slow with long media. On a media it decodes only up to the end of
  the range, so measure the media when you can.
- Not available yet: keyframe editing, speed changes, transitions between
  two adjacent clips (only one edge against what is below), compound
  clips, pasting properties, OTIO export, playback control, a ripple
  (insert) mode for `insert_clip`.
