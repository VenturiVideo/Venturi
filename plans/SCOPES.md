# SCOPES — Color window with video scopes (phase 4)

Phase 4 of plans/FLOAT_INTERMEDIATES.md, after the color correction filter
(plans/COLOR_CORRECTION.md). Branch: `feature/scopes`, from
`feature/color-correction`.

**Status:** in progress. Decisions (2026-10-08): the viewer's composed
frame (§2), all four scopes in the first version (§3).

---

## 1. What the user gets

A **Color window** (View menu, and a button in the inspector's color
correction header): a floating `egui::Window` like the mixer, open state
persisted in `settings.panels` like `mixer_open`.

- **Top: scopes**, two slots side by side (one when the window is
  narrow), each with a dropdown: Waveform, RGB parade, Vectorscope,
  Histogram. Default: Waveform + Vectorscope.
- **Bottom: the color correction** of the primary selected clip, the same
  `grade_panel::grade_section` as the inspector (four wheels per row when
  the window is wide). If the clip has none: an "Add color correction"
  button. No clip selected: scopes only.
- While the window is open, the inspector's color correction section
  shows one line, "Open in the Color window", instead of the controls.

The scopes follow the viewer: whatever it shows (playback, scrubbing,
paused), they show, updated with each new frame.

## 2. What is measured (decided)

The **viewer's composed frame**: the final picture of the timeline at the
playhead, all tracks and adjustments included — what DaVinci shows on its
Color page, and what the export will contain. Measured on the viewer's
texture, so at preview resolution (proxies included when on): plenty for
scopes, and free (already on the GPU, no extra render).

Alternative: only the selected clip, isolated. Rejected unless asked:
needs a second render per frame and hides what the adjustments above do.

## 3. Scopes (decided: all four)

| Scope | Shows | Bins |
|---|---|---|
| Waveform | luma (Rec.709) per column, 0–100 % | width × 256 |
| RGB parade | R, G, B waveforms side by side | 3 × width × 256 |
| Vectorscope | Cb/Cr of every pixel, with R/Mg/B/Cy/G/Yl targets and skin-tone line | 256 × 256 |
| Histogram | R, G, B (and luma) level counts | 4 × 256 |

All four are the same machinery (count pixels into bins, then draw the
bins), so adding the last two costs little once the first is done.

## 4. GPU implementation (`crates/vv-render/src/scopes.rs`)

- **Accumulate** (compute, one invocation per pixel, `textureLoad` of the
  viewer texture): `atomicAdd` into a `storage` buffer of `u32` bins.
  Waveform columns are downsampled to the scope's width (≤ 512).
  WebGPU core features only (compute + storage atomics), already relied on
  by `rgba_to_i420.wgsl`, so the Raspberry Pi (v3dv) is fine.
- **Draw** (fragment, full-screen triangle into an `Rgba8Unorm` texture
  the size of the slot): each bin's count to a brightness,
  `1 − exp(−k · count / expected)` with `expected` the count of a uniform
  picture (e.g. `height / 256` for the waveform), so the trace has the
  same density whatever the frame size. Parade and histogram tinted per
  channel.
- **Graticule** (IRE lines, vectorscope targets and skin line, labels)
  drawn by egui's painter over the image: crisp at any size, no text on
  the GPU.
- Shown through `register_native_texture` /
  `update_egui_texture_from_wgpu_texture`, like the viewer.
- Computed only while the window is open and only when the viewer frame
  changed. Buffers and textures reused across frames.

`Scopes` is a struct owned next to the `Compositor` (same device), with
`analyze(&wgpu::Texture, &[ScopeSlot]) -> [wgpu::Texture]`.

## 5. App (`crates/vv-app/src/color_window.rs`)

- Window, slots, dropdowns; slot choice persisted in settings.
- The grade edits from the window and from the inspector go through the
  same code: the per-target application now inline in
  `properties_panel.rs` (filter section, the `KeyframeEdit` match) moves
  into a function both call.
- View menu entry + inspector header button; locales en/it.

## 6. Tests

- vv-render, headless, on known frames:
  - flat grey → waveform: every column one bright bin at its level;
  - horizontal gradient → waveform: a diagonal;
  - pure red / green / blue → vectorscope: the trace on its target;
  - gradient → histogram: flat, counts summing to the pixel count;
  - parade of a red frame: R high, G and B at 0.
- Both precisions are irrelevant here (the input is the 8-bit viewer
  texture) — one run.
- App: the shared grade-application function (targets, keyframe or
  default, reset, preset) unit-tested once instead of through two UIs.

## 7. Order of work

1. vv-render `Scopes`: accumulate + draw for the waveform, tests.
2. Vectorscope, then parade and histogram, tests.
3. Color window with the scope slots fed from the viewer texture.
4. Grade application factored out; wheels in the window; inspector line.
5. Menu, settings, locales; `cargo test --workspace`, clippy, manual check.
