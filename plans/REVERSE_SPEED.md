# REVERSE_SPEED — clips playing backwards

**Status:** plan, nothing implemented.

A clip plays its source range backwards, at any constant speed (Resolve's
"Reverse speed" checkbox in Change Clip Speed; OTIO `LinearTimeWarp` with a
negative `time_scalar`, today a `SpeedNotApplied` warning on import).

Builds on CLIP_SPEED (constant `speed`) and on the freeze frame, which
already split *where keyframes are evaluated* (`Clip::source_frame_at`) from
*which picture is shown* (`Clip::picture_frame_at`).

---

## 1. Model (`vv-core`)

**Decision: `speed` stays positive, reverse is a picture mapping.** A
negative `Rational` in `rate` would break every invariant that assumes
`source_in < source_out` (trims, slip, split, `scale_round`, render-ahead
ranges, the mixer).

- Replace `Clip::freeze: Option<FrameIdx>` with one enum, so freeze and
  reverse cannot both be set:

  ```rust
  pub enum Picture {
      Forward,
      Freeze(FrameIdx),
      /// Shows `axis - source_frame_at(t)`.
      Reverse { axis: FrameIdx },
  }
  ```

  `serde(default)` = `Forward`. Projects saved with `freeze: Some(f)` load
  through a `deserialize_with` or a `#[serde(alias)]` shim.
- Why an `axis` stored in the clip and not a mirror computed from
  `source_in..source_out`: trim, split and ripple move `source_offset`
  together with `timeline_start`, so `source_frame_at(t)` does not change
  for the frames that stay. Every `f(source_frame_at(t))` with a fixed `f`
  survives those edits without special cases. A mirror computed from the
  trim bounds would change the picture at the far end on every trim.
- Making a clip reverse: `axis = source_in + source_out - 1`, so it shows
  the same frames in the opposite order.
- **Keyframes stay on `source_frame_at`**, i.e. in timeline order: an
  animation keeps playing forward on a reversed clip (as in Resolve). The
  keyframe editor, properties panel and color window need no change.
- Bounds of the picture: `0 <= axis - s < duration_frames`, so
  `s ∈ [axis - duration + 1, axis]`. `edit::trim_range` and
  `edit::slip_range` get a reverse branch that translates the media bounds
  through the mirror. Slip moves the picture the other way from
  `source_offset`, so the UI negates the delta.
- `media_secs_at` (audio) gets the same mirror in seconds:
  `axis_secs - forward_secs`.

## 2. Video decode — the expensive part

Every decoder path today reads forward. A reverse segment needs its frames
from the high end down, and long-GOP media can only be decoded forward from
a keyframe.

- **`WantedRange`** gets `reversed: bool`. `timeline_position_of` mirrors
  for it, so the cache evicts by the real distance from the playhead.
- **Render-ahead (preview).** For a reversed segment, timeline-forward means
  source-backward. Reuse `chunk_behind_segments_near_to_far`, which
  already decodes chunks starting from the high end. Each chunk: seek to the
  keyframe `<=` chunk start, decode up to the chunk end, cache everything.
  Align chunks to the GOP with `Decoder::landing()` so each GOP is decoded
  once. `position_decoder` needs a reverse path: always seek per chunk,
  never "already past it".
- **Memory.** A chunk must be fully cached before its last frame (the first
  one shown) is displayed: about 3 MB per 1080p frame, so a 2 s GOP at
  60 fps is about 370 MB. Cap the chunk at the cache budget. When a proxy
  exists, prefer it: proxies are all-intra, so reverse decoding costs the
  same as forward.
- **Export** (`vv-session/src/export.rs`, sequential decoder per clip): a
  reverse reader that decodes one GOP-aligned chunk into a `Vec`, serves it
  backwards, then seeks to the previous chunk. Peak memory is one chunk per
  reversed clip being exported.
- **Compound clips.** `picture_frame_at` already gives the nested frame.
  `clipped_media_segments` recursion passes `reversed` down, so the nested
  segments are decoded backwards too.

## 3. Audio (`vv-audio`)

- `ClipSpan` reads the source range as today, then the mix walks the buffer
  backwards (`step` negative, or reverse the decoded window once).
- Pitch correction: reverse the window first, then `stretch_samples` (the
  stretcher does not care about direction).
- `MixBufferCache` windows: the window for timeline range `[a, b)` is source
  range `[p(b), p(a)]`, reversed.
- Timeline waveform and compound waveforms: mirror the peaks.

## 4. UI (`vv-app`)

- "Reverse" checkbox in the Clip speed dialog (`speed_dialog.rs`), carried
  by `SetClipSpeed` (new field) so speed and direction are one undo step.
  Same for the retime bar's presets menu.
- Clip label: `◀ 100%` (or `-100%`) next to the name.
- Paste attributes: "Speed" also copies the direction.
- MCP: `reverse: true` in the clip JSON; the speed tool takes it as a flag.

## 5. OTIO

- Import: `time_scalar < 0` gives `speed = |time_scalar|` and `Reverse`.
  **Open question:** which end of `source_range` Resolve treats as the first
  frame shown. Settle it with a real Resolve export (a clip with Reverse
  speed checked, frame numbers burned in) before writing the axis formula.
- Export: `LinearTimeWarp` with negative `time_scalar`, plus the axis in
  `metadata.venturi` (like `freeze`) so our own files come back exact.

## 6. Steps

1. Model: `Picture` enum replacing `freeze` (with the serde shim),
   `picture_frame_at` mirror, `trim_range`/`slip_range` branches. Unit tests
   for trim, split, slip and ripple on reversed clips (picture unchanged on
   the frames that stay).
2. Export path (reverse chunk reader). It is correct before it is fast and
   can be checked frame by frame against a forward export.
3. Render-ahead reverse chunks plus cache distance.
4. Audio mirror and waveform.
5. UI, paste attributes, MCP.
6. OTIO import/export, after the open question in §5 is settled.

Effort: high. Steps 2–3 are most of it; 1, 4, 5 and 6 are medium or small.
