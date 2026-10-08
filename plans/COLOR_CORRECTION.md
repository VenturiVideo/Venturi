# COLOR_CORRECTION — the color correction filter (phase 3)

Phase 3 of plans/FLOAT_INTERMEDIATES.md: a DaVinci-style color correction
filter replacing `FilterKind::Grayscale`. Builds on the float intermediates
(phase 1) and the per-project precision (phase 2).

**Status:** implemented, waiting for the manual check in the app. Decisions (2026-10-08): scalar tracks (§2.1),
fixed softness (§3), Grayscale converted on load (§5).
Branch: `feature/color-correction`, from `feature/float-intermediates`.

---

## 1. What the user gets

One filter, "Color correction", in the Effects panel, droppable on any clip
and on adjustment layers (which then grade everything below for free).
Controls, in the inspector:

- Four wheels — **Shadows**, **Midtones**, **Highlights**, **Offset**
  (global) — each a disc with a draggable puck (color shift) plus a
  **luminance** slider under it, like Resolve's Log wheels.
- **Saturation** per range (shadows / midtones / highlights) and a global
  one.
- **Range**: two thresholds, shadows/midtones and midtones/highlights.
- Presets: *Neutral*, *Black & white* (global saturation 0).
- Every control keyframable, with the usual diamond and prev/next arrows.

The Effects panel keeps a "Black & white" entry: it adds a color
correction with the *Black & white* preset, so the old one-click B&W is
still there.

---

## 2. Data model

### 2.1 Parameters as scalar tracks (decided)

The FLOAT_INTERMEDIATES outline suggested `Keyframed<[f32; 3]>` per wheel.
Recommended instead: every parameter is a scalar `Keyframed<f32>`, indexed
by an enum, exactly like `TransformTracks`/`TransformParam`
(`crates/vv-core/src/model.rs:620-760`).

```rust
pub enum GradeParam {
    ShadowsX, ShadowsY, ShadowsLuma, ShadowsSat,
    MidtonesX, MidtonesY, MidtonesLuma, MidtonesSat,
    HighlightsX, HighlightsY, HighlightsLuma, HighlightsSat,
    OffsetX, OffsetY, OffsetLuma, Saturation,
    LowRange, HighRange,
}
pub struct GradeTracks { params: Vec<Keyframed<f32>> } // ALL.len() entries
```

Why: with scalar tracks the following work unchanged or with one generic
arm instead of new code paths for an array type:
- keyframe editor curves (`scalar_keyframes`, `scalar_value_at`) — a
  `[f32; 3]` track would show diamonds only, like `Color`;
- `EffectStack::for_each_f32_track` (drop/shift/rescale keyframes on trim,
  speed change, split);
- `paste_attributes` remap;
- `KeyframeValue`/`KeyframeTarget` stay `Copy` with an `f32` payload:
  `KeyframeTarget::Grade(GradeParam)`, `KeyframeValue::Grade(GradeParam, f32)`.

A wheel's X/Y are keyed together, like position X/Y (the twin pairing at
`keyframe_editor.rs:556`).

Old files with fewer params pad with defaults (custom `Deserialize`, as
`TransformTracks` does), so adding a control later is not a breaking change.

### 2.2 Where it lives

`ClipFilter` gets `#[serde(default)] pub grade: GradeTracks` (used only by
`ColorCorrection`, like `radius` is used only by blurs). `FilterValue` gets
`grade: GradeValue` (`[f32; GradeParam::COUNT]`, `Copy + PartialEq` — needed
by `OwnedLayer` equality in `frame_provider.rs`).

### 2.3 Value ranges and defaults

| Param | Range | Neutral |
|---|---|---|
| wheel X/Y | unit disc (clamped to radius 1) | 0, 0 |
| luminance | −1 … +1 | 0 |
| saturation (per range, global) | 0 … 2 | 1 |
| LowRange / HighRange | 0 … 1, Low < High | 0.33 / 0.66 |

---

## 3. Math (shader, `apply_grade` in `transform.wgsl`)

On the gamma-encoded RGB the pipeline already works in, clamp to [0, 1] at
the end (FLOAT_INTERMEDIATES §2.3: no headroom yet).

1. `Y = dot(rgb, Rec.709)`.
2. Range weights, soft and summing to 1:
   `ws = 1 − smoothstep(L − s, L + s, Y)`,
   `wh = smoothstep(H − s, H + s, Y)`, `wm = 1 − ws − wh`
   (`s` = softness, fixed: `0.5 × min(L, H − L, 1 − H)`; a user parameter
   can be added later without breaking files, see §2.1).
3. Wheel → RGB shift with zero luma: the puck (x, y) is (Cb, Cr) scaled
   by a strength constant, converted with the Rec.709 matrix at Y = 0.
   `shift = Σ w_r · (chroma_r + luma_r) + (chroma_offset + luma_offset)`.
4. `rgb += shift`.
5. Saturation: `sat = (Σ w_r · sat_r) · saturation`;
   `rgb = Y' + (rgb − Y') · sat` with `Y'` the luma after step 4.
6. Clamp.

Neutral parameters are an exact identity (tested).

### 3.1 GPU data

The per-slot scalar of `filter_params` cannot carry 18 values. New uniform
`GradeUniform` at **binding 8**, built per pass like `MaskUniform`
(binding 7, `bind_group_for`). A clip has at most one filter per kind
(`filter_mut` finds by kind), so a pass needs at most one grade; the
`ColorCorrection` slot in `filters` keeps its order in the chain (id 1,
the old Grayscale id), the params come from binding 8.

---

## 4. UI

- **Wheel widget** (new, `crates/vv-app/src/grade_wheel.rs`): painted
  disc with a hue ring, puck dragged with the pointer (fine drag with Shift),
  double-click resets. Built on the same `allocate_exact_size` +
  `click_and_drag` pattern as `mixer_panel::knob` and `eq_panel::graph`.
- **Inspector section** for `ColorCorrection`: the four wheels in a 2×2
  grid (one column under ~360 px), luminance slider and keyframe diamond
  under each; then saturation rows, range rows (each with diamonds via
  `param_row`/`RowKeyframe`). Preset combo in the section header.
- `FilterPanelInfo` (`main.rs:200`) gets the grade values and per-param
  `RowKeyframe`s.
- Edits go through the existing filter path (`set_filters` /
  `upsert_keyframe`, properties_panel.rs:2078-2245), with a new arm for
  `KeyframeTarget::Grade` in the match at :2205 (its `_ =>` fallback must
  stop catching everything).
- Locales (en + it): `filter.color_correction`, `filter.black_and_white`,
  `props.grade_*`.

---

## 5. Removing Grayscale

FLOAT_INTERMEDIATES §5 planned no migration. Changed (decided): OTIO
import deserializes the whole `EffectStack` with `unwrap_or_default()`
(`otio/import.rs:404`), so a failing Grayscale would silently drop **all**
that clip's effects there. Instead `Grayscale` loads as a `ColorCorrection`
with the *Black & white* preset (projects and OTIO alike).

Touch points (from the mapping): `ALL_FILTER_KINDS`, `filter_label`,
`filter_shader_id`, the shader branch, the tests using Grayscale
(vv-render compositor tests, vv-session frame_provider test,
vv-core persistence test), plans mentioning it.

---

## 6. Tests

- **Shader math** (vv-render, CPU reference in the test): neutral is
  identity (±1); global saturation 0 gives R=G=B equal to Rec.709 luma;
  shadows luminance lifts a dark solid and leaves a bright one; equal
  luminance on the three ranges equals the same on Offset (weights sum to
  1); a wheel push shifts hue with luma unchanged (±1).
- **On an adjustment layer**: grades the stack below, not above.
- **Model**: `GradeTracks` serde round-trip; an old file with fewer params
  loads with neutral defaults; keyframed X interpolates; trim/speed/split
  move grade keyframes (`for_each_f32_track`).
- **Commands**: keyframe upsert/remove/move on a `Grade` target, undo.
- **Paste attributes**: filters with a grade are pasted with remapped
  keyframes.
- Both precisions for the shader tests (phase 2).

---

## 7. Order of work

1. Model + serde + commands + tests (vv-core).
2. Shader + uniform + compositor tests (vv-render).
3. Remove Grayscale everywhere, fix tests.
4. Inspector section with plain sliders for every param (usable end to
   end), keyframe editor rows.
5. Wheel widget replacing the X/Y sliders.
6. Presets, Effects panel entries, locales.
7. `cargo test --workspace`, clippy, manual check in the app.
