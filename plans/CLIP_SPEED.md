# CLIP_SPEED — constant clip speed

**Status:** implemented (all steps in §5).

Speed up / slow down a clip: its source range stays the same, its length on
the timeline changes (`len = len_at_100% / speed`). Constant speed only:
no ramps, no reverse (see REVERSE_SPEED.md). Freeze frames are a separate
picture mapping (`Clip::freeze`, `Clip::picture_frame_at`).

Decisions (agreed with the user):
- **Audio**: pitch preserved by default (`pitch_correction`, rubberband
  stretch, same pipeline as the "a" key); turned off per clip it becomes
  varispeed (pitch follows the speed).
- **Length** (`vv_core::SpeedFit`):
  - retime bar (edge drag and presets): exactly like a trim of the end —
    lengthening overwrites what follows, shortening leaves a gap, snapping
    included (`Resize` + `make_room_for_ranges`);
  - "Clip speed…" dialog: "Push following clips" (default) shifts every
    clip starting at or after the old end on all unlocked tracks (same rule
    as `RippleDeleteGap`, keeps A/V sync); off, the clip keeps its length
    and shows more/less of the source, clamped to the media;
  - paste properties ("Speed", with the pitch correction): ripple, so
    adjacent pasted clips never overwrite each other.
- The speed of the retime bar sits at the center of the visible part of the
  clip.

---

## 1. Model (`vv-core`)

- `Clip::speed: Rational` (source time per timeline time, `2/1` = 200%) and
  `Clip::pitch_correction: bool`, both `serde(default)`.
  `EffectStack::speed: Keyframed<f32>` goes away: it was never played back,
  `rate` needs an exact fraction, and old files still load (unknown fields
  are ignored).
- Invariant: `rate = conform_rate(timeline_fps, media_fps) / speed`. All the
  mapping already goes through `rate` (`source_frame_at`, `source_in/out`,
  trims, `render_ahead`, export), so video follows with no other change.
  Places computing `rate` from the fps apply the speed
  (`refresh_clip_rates`, paste across fps, OTIO import).
- `media_secs_at` multiplies by `speed` (audio and waveform).
- `SetClipSpeed` command: new speed for a set of clips (the UI passes whole
  linked groups), `source_in` and `timeline_start` fixed; ripple or
  keep-length as above. Undo restores the snapshot of the tracks.
- Speed only on `Media` clips (compound included); generators have no
  source time to remap.

## 2. Audio (`vv-audio`, `vv-app`)

- `MixClip` gains `speed` (buffer frames per output frame) and the media
  position for the gain keyframes. `mix_range` reads with linear
  interpolation when `speed != 1`.
- Pitch correction: `AudioSource::stretched(buffer, range, tempo)`: the
  clip's source range is stretched with `vv_audio::stretch_samples`.
  Default implementation is synchronous (export); `MixBufferCache` runs it
  on its own worker thread and the clip is silent (`Pending`) until it is
  ready — never an approximate sound in its place.

## 3. UI

- **Speed handles** (Ctrl+R, `Action::RetimeControls`, also in the clip
  context menu): a bar in the accent colour on top of the selected clips with the speed
  (`100% ▾`, presets menu) and `×` to close. While shown, dragging the right
  edge of the clip changes speed instead of trimming: the linked group
  follows, ripple on release.
- **Clip speed…** in the context menu: a dialog with the percentage and
  presets, "Keep pitch" and "Push following clips" (on by default).
- The clip label shows the speed when it is not 100%.

## 4. OTIO

- Import: `LinearTimeWarp.time_scalar > 0` becomes `Clip::speed` (verified
  on Resolve's file: `source_range.start` is media time, `duration` is
  timeline time — the track positions are sums of durations).
- Export: `LinearTimeWarp` from `Clip::speed`, `source_range.start` in media
  time; `metadata.venturi` carries the exact `speed` and `pitch_correction`.

## 5. Steps

| # | Step |
|---|------|
| 1 | Model: fields, invariant, `media_secs_at`, `SetClipSpeed` + tests |
| 2 | Mixer: varispeed + pitch-corrected stretch (preview async, export sync) + tests |
| 3 | OTIO import/export + tests |
| 4 | UI: action Ctrl+R, retime bar and drag, context menu, dialog |
| 5 | Headless check in the container |
