# FLOAT_INTERMEDIATES — float working textures, then color correction

Groundwork for a DaVinci-style color correction filter: move the
compositor's intermediate textures from `Rgba8Unorm` to `Rgba16Float`, so a
chain of passes (per-clip grade + blur + adjustment layer + compound clips)
no longer rounds to 8 bits at every step.

**Status:** phase 1 implemented on `feature/float-intermediates`, plus the
phase 2 project setting (Project → Project settings → Processing precision), brought forward so
the manual test (§4) can A/B without switching branch.

**Sequence (decided):**

1. **Phase 1** (this plan, detailed): float 16 everywhere, hard-coded, no
   setting. Then **manual test** by the user on an Intel i5 iGPU and a
   Raspberry Pi.
2. Only after the manual test: phase 2 (project setting 8/16 bit), then
   phase 3 (color correction), phase 4 (scopes). Outlined at the end; each
   gets a detailed plan when its turn comes.

---

## 1. Why

Shader math is already `f32`; precision is lost only where an image is
**written** to a texture. Today every intermediate is `Rgba8Unorm`
(`OUTPUT_FORMAT`, `crates/vv-render/src/compositor.rs:14`), so each pass of a
chain quantizes again. A smooth gradient (typically a blur output) quantized
to 8 bits and then stretched by a later tonal operation (Exposure, a future
grade, on the clip or on an adjustment layer above) shows visible bands.

Not addressed here: sources are uploaded as 8-bit planes (`PLANE_FORMAT =
R8Unorm`), so 10-bit footage is still truncated at upload. Separate future
work.

---

## 2. Phase 1 design

### 2.1 Formats

- New `WORK_FORMAT = Rgba16Float` for every texture the compositor renders
  into while composing: the composite target, scratch textures, blur
  ping-pong, backdrop copies, compound-clip textures
  (`render_layers_to_owned_texture_transparent`, used as
  `LayerContent::Texture` by the parent timeline, so nesting keeps
  precision).
- What leaves the compositor stays 8 bit, so callers do not change:
  - `render_layers_to_texture` (preview, sampled by egui via
    `register_native_texture` / `update_egui_texture_from_wgpu_texture` in
    `crates/vv-app/src/main.rs`): returns an `Rgba8Unorm` texture produced
    by a final **resolve pass** (float → 8 bit, with dithering).
  - `render_layers_rgba_transparent` / `read_rgba_texture`: read back the
    resolved `Rgba8Unorm` texture, byte layout unchanged.
  - `render_layers_i420` (export): `rgba_to_i420.wgsl` reads the float
    work texture directly (`texture_2d<f32>` already accepts it) and
    dithers in its `to_byte`, so the export is not dithered twice (RGB, then
    again in YUV).
- `Rgba16Float` is renderable, blendable and filterable in WebGPU core, but
  check `adapter.get_texture_format_features(Rgba16Float)` at compositor
  creation anyway (GL backend / old drivers): if `RENDER_ATTACHMENT`,
  `BLENDABLE` or `FILTERABLE` is missing, fall back to `Rgba8Unorm` for
  `WORK_FORMAT` with a `log::warn!`. That also covers `new_headless`.

### 2.2 Pipelines

The WGSL does not change: fragment shaders return `vec4<f32>` and the GPU
converts to the target format. Only `ColorTargetState.format` changes
(`compositor.rs:693` transform/blend pipelines, `:734` blur pipeline): pass
the format as a parameter (the `transform_pipeline` closure already exists).
Phase 2 will instantiate them with either format, so keep the format a
runtime value, not a `const` baked into pipeline creation.

New: the resolve pipeline (fullscreen triangle, reads the work texture,
dithers, writes `Rgba8Unorm`). Small new shader, or an entry point in an
existing one if it fits naturally.

### 2.3 Clamping: precision only, no headroom (yet)

Phase 1 keeps every existing `clamp(…, 0, 1)` (Exposure at
`transform.wgsl:121`, YUV→RGB at `:151`, unpremultiply at `:322`, blend
modes). Values stay in [0, 1]: the visible output is the same as today
apart from less banding, which keeps the change easy to validate.
Letting values exceed 1.0 between passes (highlight recovery after an
Exposure push) is a deliberate later decision, not part of this phase —
blend modes like ColorDodge/Divide/Screen assume [0, 1].

### 2.4 Dithering

- Deterministic spatial noise from the pixel coordinates (hash-based,
  triangular PDF, ±1 LSB), no time component: same frame → same bytes, so
  exports are reproducible and tests stay deterministic; no temporal
  flicker for the encoder to spend bitrate on.
- Applied only in the two 8-bit exits: the resolve pass and
  `rgba_to_i420`'s `to_byte`.
- Alpha is not dithered.
- A value already within 1/16 step of an 8-bit level (what f16 storage
  leaves of an exact level) is not dithered: flat colors and black bars
  stay clean, no noise for the encoder. `shaders/dither.wgsl`, prepended to
  both exits.

### 2.5 Pools and memory

- `take_sized` already filters by format: check that scratch and output
  pools can hold both the float work textures and the 8-bit resolved ones
  without handing one out as the other.
- A 4K float texture is ~66 MB (vs ~33 MB): keep the pool from holding more
  float textures than needed.

---

## 3. Tests

- Existing compositor tests (`crates/vv-render/src/tests/compositor.rs`,
  `crates/vv-session/src/tests/frame_provider.rs`) read 8-bit bytes: run
  them first and look at each failure. Expected differences are ±1 from
  dithering; where a test asserts exact values, give it a ±1/±2 tolerance
  rather than disabling dithering (test the real path). Any larger
  difference is a bug, not a tolerance to widen.
- New tests (in `crates/vv-render/src/tests/`, never inline — see CLAUDE.md):
  - **Precision across passes:** a chain that bands at 8 bit, e.g. a smooth
    gradient layer → Exposure −4 → adjustment layer with Exposure +4.
    Assert the output has (almost) as many distinct levels as the input
    gradient; the same scene with 8-bit intermediates would collapse to a
    few. Ideally the test fails on `master`.
  - **Dither determinism:** same frame rendered twice → identical bytes.
  - **Dither is unbiased:** a flat mid-gray layer resolves to a mean equal
    to the input within a small epsilon.
  - **Fallback:** the format-selection logic picks `Rgba8Unorm` when the
    features are missing (test the pure selection function, not a real
    adapter).
- `cargo test --workspace` and `cargo clippy --workspace` clean before
  handing over.

---

## 4. Manual test (user, before phase 2)

Build release from the branch and from `master`, same project files.

Machines: Intel i5 iGPU; Raspberry Pi (check first that wgpu picks the
Vulkan v3dv backend, and note what `adapter_name` reports).

Scenes (1080p and 4K timeline):

1. **Plain cuts**, no effects: baseline for playback smoothness and export
   time.
2. **Heavy blur:** Gaussian blur radius 250 on a full-frame clip, plus a
   Box blur on another track. Most sensitive to bandwidth.
3. **Adjustment + blur + Exposure chain:** Exposure −3 on the clip,
   Gaussian blur, adjustment layer above with Exposure +3. Visual A/B for
   banding (sky, walls, out-of-focus areas).
4. **Compound clip** containing scene 3, with another adjustment above it.

For each: preview playback (dropped frames / stutter at 1×), scrubbing
responsiveness, export wall time, GPU memory (`intel_gpu_top` / `free -m`
on the Pi, where GPU memory is shared RAM), and the banding A/B on scene 3.

Decision after the test:
- No meaningful cost → phase 2 still adds the setting (decided), with
  16 bit as default.
- Meaningful cost on the weak machines → same, but the setting description
  says so; consider whether blur passes deserve a special case (only safe
  when nothing tonal follows them — see §1).

---

## 5. Later phases (outline only)

**Phase 2 — project setting.** Per-project (not app preference: it changes
the export, so the same project must export the same on every machine).
`serde(default)` = 16 bit for new and existing projects. Preview and export
always use the same precision. UI label in plain words (e.g. "Processing
precision: High (recommended for color correction) / Standard (lighter)").
Changing it recreates the pipelines and invalidates any cached rendered
frames. Compositor tests run in both modes.

**Phase 3 — Color correction filter.** Replaces `FilterKind::Grayscale`
(no migration: old projects using it will fail to load; the release notes
must say so explicitly). Black & white becomes a preset (saturation 0).
Controls in the style of Resolve's Log wheels: shadows / midtones /
highlights / global, each with color offset + luminance, saturation per
range, configurable range thresholds with soft (smoothstep, weights summing
to 1) masks. Every parameter keyframable via `Keyframed` (wheels as
`Keyframed<[f32; 3]>` with a `Lerp` impl). Works on adjustment layers for
free. Needs more than the one scalar per slot of today's
`filter_params` uniform: a dedicated uniform/storage buffer.

**Phase 4 — Scopes.** Waveform + vectorscope (then RGB parade, histogram),
computed on the GPU from the composed frame, independent of the filter.
Decided (2026-10-08): they live in a **Color window**, a floating
`egui::Window` like the mixer (`mixer_panel.rs:143`, open state and size
persisted as `settings.panels.mixer_open`/size are). It shows the color
correction of the selected clip with the wheels in one row (the
responsive `grade_panel::grade_section`, which already goes to four per
row when wide) next to the scopes, a small "Color page" in Resolve's
style. While it is open the inspector shows only a line pointing to it,
so the controls are never in two places at once. Opened from the
inspector's color correction header and from the View menu.

**Not planned now:** OpenColorIO (heavy C++ dependency, GLSL-only shader
generation, no current need for log/ACES workflows). A `.cube` 3D LUT
filter is the lightweight alternative if camera/look LUTs are requested;
10-bit source upload (`R16Unorm` planes) is the other follow-up.
