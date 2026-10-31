# Clip Pitch Shift

Select an audio clip and open **Properties → Audio → Pitch Shift**. The
effect changes its pitch without moving it or changing its timeline duration.
Edits apply to all selected audio clips.
Changing one control preserves the other settings of each selected clip.
Presets and reset replace the full pitch configuration on all selected clips.

| Control | Behavior |
| --- | --- |
| Enabled | Bypasses the effect while retaining its settings. |
| Semitones | Coarse transposition from −24 to +24 semitones. Twelve semitones make an octave. |
| Cents | Fine adjustment from −100 to +100 cents. One hundred cents make a semitone. |
| Preserve formants | Keeps vocal resonances closer to the original while changing the fundamental pitch. Off gives the natural transposed timbre. |
| Presets | Original pitch, octave down/up and fifth down/up. |
| Reset pitch | Restores the original pitch and default settings. |

The two transposition controls add together. Settings are constant over a
clip; they do not have keyframes. Edits, bypass, presets and reset support
undo/redo. Copy/paste preserves the settings, and **Paste attributes → Audio
→ Pitch Shift** copies them independently of volume. Audio library presets
can also include this attribute.

## Speed and rendering

The pitch offset is relative to what the clip would otherwise play. With
**Keep pitch** enabled, a 200% clip shifted up twelve semitones plays an
octave above the source. With **Keep pitch** disabled, the same clip plays
two octaves above the source: one from speed and one from Pitch Shift.

Preview and export use the same Rubber Band processing through FFmpeg.
Pitch shifting uses smooth transient handling to avoid phase-reset artifacts
on sustained tones. This favours continuity and may soften very sharp attacks.
The preview prepares the processed clip in the background and caches it;
the clip can be silent while this preparation runs. Changing its source
range, speed, pitch or formant setting requests a new processed buffer.
Processing runs outside the realtime audio callback. Compound clips are
supported, including pitch effects inside the nested timeline.

Project files and Venturi's OTIO metadata preserve the settings. Other
editors do not receive a native pitch effect from this metadata.

## Build requirement

FFmpeg must include the `rubberband` audio filter, also used by Venturi's
existing pitch-preserving speed changes. Check the FFmpeg installation with:

```sh
ffmpeg -hide_banner -h filter=rubberband
```

The FFmpeg libraries linked into Venturi must include the filter too. A
separately installed command-line executable does not change those libraries.
No additional Rust dependency is needed for this effect.
