// Transform (crop, zoom, rotation, position) and YUV420->RGB conversion
// in a fullscreen triangle, without a vertex buffer.

struct TransformUniform {
    // left, top, right, bottom in normalized [0,1] coordinates on the source.
    crop: vec4<f32>,
    // zoom.x, zoom.y, position.x, position.y
    zoom_pos: vec4<f32>,
    // x/y: letterbox factors (>1 on the uncovered axis). z: rotation in
    // radians, clockwise. w: crop softness in fractions of the source
    // (negative towards the inside).
    fit_rot: vec4<f32>,
    // anchor.x, anchor.y (zoom and rotation pivot, in fractions of the
    // output frame from the center of the clip), flip.x, flip.y (0 or 1).
    anchor_flip: vec4<f32>,
    // x: matrix (0=BT.601, 1=BT.709, 2=BT.2020). y: 1 if full range.
    // z: output aspect, to rotate without deforming.
    // w: 0 video, 1 solid color, 2 solid color with coverage in the Y plane
    // (text), 3 premultiplied RGBA texture in place of the planes.
    color: vec4<f32>,
    // RGBA of the solid color layer, in place of the Y/U/V planes. Adjustment
    // layer: the clear color of the timeline.
    solid: vec4<f32>,
    // x: opacity of the whole layer (clip fades and clip
    // opacity). y: id of the compositing method (see `blend_shader_id`).
    // z: opacity of an adjustment layer (`x` is then 1). w: 1 if it is one.
    extra: vec4<f32>,
    // x: 1 if U/V are interleaved in u_tex (NV12), v_tex then unused.
    planes: vec4<f32>,
    // Shader ids of the clip's active filters, in order of application
    // (0 = empty slot); see `filter_shader_id` in compositor.rs, the only
    // place that knows which `FilterKind` each id corresponds to.
    filters: array<vec4<f32>, 2>,
    // The scalar parameter of each slot of `filters`.
    filter_params: array<vec4<f32>, 2>,
};

@group(0) @binding(0) var y_tex: texture_2d<f32>;
@group(0) @binding(1) var u_tex: texture_2d<f32>;
@group(0) @binding(2) var v_tex: texture_2d<f32>;
@group(0) @binding(3) var input_sampler: sampler;
@group(0) @binding(4) var<uniform> transform: TransformUniform;
// Per-pixel coverage (1x1 opaque for a layer without real transparency, e.g.
// a decoded video — see YuvFrame::alpha in compositor.rs).
@group(0) @binding(5) var a_tex: texture_2d<f32>;
// Copy of what has already been composed underneath, premultiplied: needed only
// by the compositing methods other than Normal, which must read the backdrop
// (the pipeline's fixed alpha blending is not enough). With Normal it is a
// 1x1 placeholder, never sampled.
@group(0) @binding(6) var backdrop_tex: texture_2d<f32>;

// See `MaskData`/`MaskUniform` in compositor.rs.
struct MaskData {
    shape_mode: vec4<f32>,
    center_size: vec4<f32>,
    params: vec4<f32>,
    points: vec4<f32>,
};

struct MaskUniform {
    header: vec4<f32>,
    masks: array<MaskData, 8>,
    points: array<vec4<f32>, 128>,
};

@group(0) @binding(7) var<uniform> mask_uniform: MaskUniform;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // The "fullscreen triangle" trick: 3 vertices covering the whole
    // viewport without needing a vertex buffer.
    let x = f32((vertex_index << 1u) & 2u);
    let y = f32(vertex_index & 2u);

    var out: VertexOutput;
    out.clip_position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

// Kr/Kb coefficients for the requested matrix (plans/REFACTOR_PIPELINE.md B3,
// see the docs of vv_media::ColorMatrix for the choice of which matrix to use
// case by case — here only the application).
fn kr_kb(matrix_id: i32) -> vec2<f32> {
    if (matrix_id == 1) {
        return vec2<f32>(0.2126, 0.0722); // BT.709
    } else if (matrix_id == 2) {
        return vec2<f32>(0.2627, 0.0593); // BT.2020
    }
    return vec2<f32>(0.299, 0.114); // BT.601 (also the fallback for unhandled matrices)
}

// Slot `i` (0..8) inside the two vec4s of `TransformUniform.filters` (or
// `filter_params`): an
// array<vec4,2> cannot be indexed linearly in WGSL, it must be unpacked.
fn filter_id_at(filters: array<vec4<f32>, 2>, i: i32) -> f32 {
    let group = filters[i / 4];
    let lane = i % 4;
    if (lane == 0) { return group.x; }
    if (lane == 1) { return group.y; }
    if (lane == 2) { return group.z; }
    return group.w;
}

// Applies a filter in sequence to `rgb`; the call order (see the
// loop in `fs_main`) is the order chosen by the user. New per-pixel filters: a new
// id (`filter_shader_id`) and a new branch here, nothing else in the pipeline.
fn apply_filter(rgb: vec3<f32>, id: f32, param: f32) -> vec3<f32> {
    if (id > 0.5 && id < 1.5) { // Grayscale
        let luma = dot(rgb, vec3<f32>(0.299, 0.587, 0.114));
        return vec3<f32>(luma, luma, luma);
    }
    if (id > 1.5 && id < 2.5) { // Exposure, `param` in stops
        // Scaled in (approximately) linear light, as a camera would.
        let linear = pow(rgb, vec3<f32>(2.2)) * exp2(param);
        return clamp(pow(linear, vec3<f32>(1.0 / 2.2)), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return rgb;
}

fn yuv_to_rgb(y_sample: f32, u_sample: f32, v_sample: f32, matrix_id: i32, full_range: bool) -> vec3<f32> {
    var y_n: f32;
    var u_n: f32;
    var v_n: f32;
    if (full_range) {
        y_n = y_sample;
        u_n = u_sample - 0.5;
        v_n = v_sample - 0.5;
    } else {
        // Limited/MPEG: codes 16-235 (luma) / 16-240 (chroma) on 8 bits,
        // already normalized to [0,1] by the sampler (16/255..235/255 etc.) —
        // re-expands to the full range before applying the matrix.
        y_n = (y_sample - 16.0 / 255.0) * (255.0 / 219.0);
        u_n = (u_sample - 128.0 / 255.0) * (255.0 / 224.0);
        v_n = (v_sample - 128.0 / 255.0) * (255.0 / 224.0);
    }

    let kkb = kr_kb(matrix_id);
    let kr = kkb.x;
    let kb = kkb.y;
    let kg = 1.0 - kr - kb;

    let r = y_n + 2.0 * (1.0 - kr) * v_n;
    let b = y_n + 2.0 * (1.0 - kb) * u_n;
    let g = y_n - (2.0 * kr * (1.0 - kr) / kg) * v_n - (2.0 * kb * (1.0 - kb) / kg) * u_n;
    return clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
}

// The output pixel in the layer's space before the fit: fractions of the
// output frame from the center of the clip, Y down. Inverse of position,
// then of rotation and zoom around the anchor.
fn layer_point(in: VertexOutput) -> vec2<f32> {
    let zoom = max(abs(transform.zoom_pos.xy), vec2<f32>(0.0001, 0.0001));
    let position = transform.zoom_pos.zw;
    let anchor = transform.anchor_flip.xy;
    let angle = transform.fit_rot.z;
    let aspect = max(transform.color.z, 0.0001);

    var q = in.uv - vec2<f32>(0.5, 0.5) - position - anchor;
    // The rotation must be done in an isotropic space, otherwise a non-square
    // frame would turn it into a shear.
    q = vec2<f32>(q.x * aspect, q.y);
    let cs = cos(angle);
    let sn = sin(angle);
    q = vec2<f32>(q.x * cs + q.y * sn, -q.x * sn + q.y * cs);
    q = vec2<f32>(q.x / aspect, q.y);
    return q / zoom + anchor;
}

fn mask_point(i: i32) -> vec2<f32> {
    let pair = mask_uniform.points[i / 2];
    return select(pair.xy, pair.zw, i % 2 == 1);
}

// Signed distance from the outline, negative inside, in layer pixels.
fn mask_distance(m: MaskData, p: vec2<f32>) -> f32 {
    let shape = m.shape_mode.x;
    if (shape > 1.5) {
        // Polygon (iq's sdPolygon): distance to the nearest edge, sign from
        // the crossings.
        let first = i32(m.points.x);
        let count = i32(m.points.y);
        if (count < 3) {
            return 1e9;
        }
        var v_prev = mask_point(first + count - 1);
        var d = dot(p - v_prev, p - v_prev);
        var s = 1.0;
        for (var k = 0; k < count; k = k + 1) {
            let v = mask_point(first + k);
            let e = v_prev - v;
            let w = p - v;
            let b = w - e * clamp(dot(w, e) / max(dot(e, e), 1e-12), 0.0, 1.0);
            d = min(d, dot(b, b));
            let above = p.y >= v.y;
            let below = v_prev.y > p.y;
            let left = e.x * w.y > e.y * w.x;
            if ((above && below && left) || (!above && !below && !left)) {
                s = -s;
            }
            v_prev = v;
        }
        return s * sqrt(d);
    }
    // Into the shape's own frame: undo the clockwise rotation (Y up).
    let angle = m.params.x;
    let d0 = p - m.center_size.xy;
    let cs = cos(angle);
    let sn = sin(angle);
    let local = vec2<f32>(d0.x * cs - d0.y * sn, d0.x * sn + d0.y * cs);
    let half_size = max(m.center_size.zw, vec2<f32>(0.0001, 0.0001));
    if (shape > 0.5) {
        // Ellipse: iq's approximation, good enough for a soft edge.
        let k0 = length(local / half_size);
        let k1 = length(local / (half_size * half_size));
        if (k1 < 1e-6) {
            return -min(half_size.x, half_size.y);
        }
        return k0 * (k0 - 1.0) / k1;
    }
    let r = min(m.params.y, min(half_size.x, half_size.y));
    let q = abs(local) - half_size + vec2<f32>(r, r);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// Combined coverage of the layer's masks at this pixel; 1 without masks.
fn mask_coverage(in: VertexOutput) -> f32 {
    let count = i32(mask_uniform.header.x);
    if (count == 0) {
        return 1.0;
    }
    let q = layer_point(in);
    let p = vec2<f32>(q.x, -q.y) * mask_uniform.header.zw;
    let aa = max(mask_uniform.header.y, 0.0001);
    var coverage = 0.0;
    for (var i = 0; i < count; i = i + 1) {
        let m = mask_uniform.masks[i];
        let d = mask_distance(m, p) - m.params.w;
        let feather = m.params.z;
        var c: f32;
        if (feather > 0.0) {
            c = 1.0 - smoothstep(-0.5 * feather, 0.5 * feather, d);
        } else {
            c = clamp(0.5 - d / aa, 0.0, 1.0);
        }
        if (m.shape_mode.z > 0.5) {
            c = 1.0 - c;
        }
        c = c * m.shape_mode.w;
        let mode = m.shape_mode.y;
        if (mode > 1.5) {
            coverage = select(min(coverage, c), c, i == 0);
        } else if (mode > 0.5) {
            coverage = select(coverage, 1.0, i == 0) * (1.0 - c);
        } else {
            coverage = max(coverage, c);
        }
    }
    return coverage;
}

// Color of the layer alone, not premultiplied.
fn shade(in: VertexOutput) -> vec4<f32> {
    let q = layer_point(in);

    let flip = vec2<f32>(
        select(1.0, -1.0, transform.anchor_flip.z > 0.5),
        select(1.0, -1.0, transform.anchor_flip.w > 0.5),
    );
    let source_uv = q * flip * transform.fit_rot.xy + vec2<f32>(0.5, 0.5);

    let crop_min = transform.crop.xy;
    let crop_max = transform.crop.zw;
    let softness = transform.fit_rot.w;

    // Outside the clip there is nothing to show (the softness towards the
    // outside must not smear the edge of the source).
    if (any(source_uv < vec2<f32>(0.0, 0.0)) || any(source_uv > vec2<f32>(1.0, 1.0))) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    // Distance from the nearest crop edge, negative outside: the softness is
    // an alpha ramp around that edge.
    let inside = min(source_uv - crop_min, crop_max - source_uv);
    let edge_distance = min(inside.x, inside.y);
    var alpha = 1.0;
    if (softness < 0.0) {
        if (edge_distance < 0.0) {
            return vec4<f32>(0.0, 0.0, 0.0, 0.0);
        }
        alpha = smoothstep(0.0, -softness, edge_distance);
    } else if (softness > 0.0) {
        if (edge_distance < -softness) {
            return vec4<f32>(0.0, 0.0, 0.0, 0.0);
        }
        alpha = smoothstep(-softness, 0.0, edge_distance);
    } else if (edge_distance < 0.0) {
        // Hard crop: the layer below shows through.
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    alpha = alpha * transform.extra.x;

    // U/V are at half resolution (4:2:0): sampling them at the same uv
    // as the Y plane with a bilinear sampler also does the chroma
    // upsampling, for free.
    let mode = transform.color.w;
    var rgb: vec3<f32>;
    var out_alpha: f32;
    if (mode > 0.5 && mode < 1.5) {
        rgb = transform.solid.rgb;
        out_alpha = alpha * transform.solid.a;
    } else if (mode > 2.5) {
        // The frame was composed in alpha-over onto a transparent
        // background: the color is already multiplied by the alpha, and
        // the alpha-over of this pass would multiply it again.
        let texel = textureSample(y_tex, input_sampler, source_uv);
        rgb = clamp(texel.rgb / max(texel.a, 1.0 / 255.0), vec3<f32>(0.0), vec3<f32>(1.0));
        out_alpha = alpha * texel.a;
    } else {
        let y_sample = textureSample(y_tex, input_sampler, source_uv).r;
        if (mode > 1.5) {
            rgb = transform.solid.rgb;
            out_alpha = alpha * transform.solid.a * y_sample;
        } else {
            let u_texel = textureSample(u_tex, input_sampler, source_uv);
            let v_planar = textureSample(v_tex, input_sampler, source_uv).r;
            let u_sample = u_texel.r;
            let v_sample = select(v_planar, u_texel.g, transform.planes.x > 0.5);
            let matrix_id = i32(transform.color.x);
            let full_range = transform.color.y > 0.5;
            rgb = yuv_to_rgb(y_sample, u_sample, v_sample, matrix_id, full_range);
            out_alpha = alpha;
        }
    }
    for (var slot = 0; slot < 8; slot = slot + 1) {
        let id = filter_id_at(transform.filters, slot);
        if (id > 0.5) {
            rgb = apply_filter(rgb, id, filter_id_at(transform.filter_params, slot));
        }
    }
    // 1x1 placeholder for a layer without real per-pixel coverage: it always
    // samples 1.0, no effect (see the docs of `a_tex`).
    out_alpha = out_alpha * textureSample(a_tex, input_sampler, source_uv).r;
    return vec4<f32>(rgb, out_alpha);
}

// B(Cb, Cs) of the separable methods, channel by channel; `id` comes from
// `blend_shader_id` in compositor.rs.
fn blend_channel(id: i32, cb: f32, cs: f32) -> f32 {
    switch (id) {
        case 1: { return min(cb + cs, 1.0); }            // Add
        case 2: { return cb * cs; }                      // Multiply
        case 3: { return cb + cs - cb * cs; }            // Screen
        case 4: {                                        // Overlay
            if (cb <= 0.5) { return 2.0 * cb * cs; }
            return 1.0 - 2.0 * (1.0 - cb) * (1.0 - cs);
        }
        case 5: { return min(cb, cs); }                  // Darken
        case 6: { return max(cb, cs); }                  // Lighten
        case 7: {                                        // Color Dodge
            if (cs >= 1.0) { return 1.0; }
            return min(cb / (1.0 - cs), 1.0);
        }
        case 8: {                                        // Color Burn
            if (cs <= 0.0) { return 0.0; }
            return 1.0 - min((1.0 - cb) / cs, 1.0);
        }
        case 9: {                                        // Hard Light
            if (cs <= 0.5) { return 2.0 * cb * cs; }
            return 1.0 - 2.0 * (1.0 - cb) * (1.0 - cs);
        }
        case 10: {                                       // Soft Light (W3C)
            var d: f32;
            if (cb <= 0.25) {
                d = ((16.0 * cb - 12.0) * cb + 4.0) * cb;
            } else {
                d = sqrt(cb);
            }
            if (cs <= 0.5) { return cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb); }
            return cb + (2.0 * cs - 1.0) * (d - cb);
        }
        case 11: { return abs(cb - cs); }                // Difference
        case 12: { return cb + cs - 2.0 * cb * cs; }     // Exclusion
        case 13: { return max(cb - cs, 0.0); }           // Subtract
        case 14: {                                       // Divide
            if (cs <= 0.0) { return 1.0; }
            return min(cb / cs, 1.0);
        }
        default: { return cs; }
    }
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let coverage = mask_coverage(in);
    let blend_id = i32(transform.extra.y);
    if (transform.extra.w > 0.5) {
        return adjusted(in, shade(in), blend_id, coverage);
    }
    var src = shade(in);
    src.a = src.a * coverage;
    // Normal: the pipeline's alpha blending takes care of it, the color comes out
    // non-premultiplied.
    if (blend_id == 0) {
        return src;
    }
    // The other modes write the already composed result in REPLACE, hence
    // premultiplied like the backdrop they read.
    let dst = textureLoad(backdrop_tex, vec2<i32>(floor(in.clip_position.xy)), 0);
    return blend_over(blend_id, src, dst);
}

// Adjustment layer (REPLACE pipeline): `src` is the stack below, transformed and
// filtered. Uncovered areas take the clear color instead of the original
// stack, then the result is mixed with the original by the opacity and the
// masks.
fn adjusted(in: VertexOutput, src: vec4<f32>, blend_id: i32, coverage: f32) -> vec4<f32> {
    let dst = textureLoad(backdrop_tex, vec2<i32>(floor(in.clip_position.xy)), 0);
    let clear = transform.solid;
    let processed = vec4<f32>(src.rgb * src.a, src.a) + vec4<f32>(clear.rgb * clear.a, clear.a) * (1.0 - src.a);
    var composed = processed;
    if (blend_id != 0) {
        let rgb = processed.rgb / max(processed.a, 1.0 / 255.0);
        composed = blend_over(blend_id, vec4<f32>(rgb, processed.a), dst);
    }
    return mix(dst, composed, transform.extra.z * coverage);
}

// `src` (not premultiplied) composed onto `dst` (premultiplied) with the
// method `blend_id`; premultiplied result.
fn blend_over(blend_id: i32, src: vec4<f32>, dst: vec4<f32>) -> vec4<f32> {
    let dst_rgb = dst.rgb / max(dst.a, 1.0 / 255.0);
    var blended = vec3<f32>(
        blend_channel(blend_id, dst_rgb.r, src.r),
        blend_channel(blend_id, dst_rgb.g, src.g),
        blend_channel(blend_id, dst_rgb.b, src.b),
    );
    // Where there is nothing underneath the blend has no backdrop to act on: there
    // the source color alone applies.
    blended = mix(src.rgb, blended, dst.a);
    return vec4<f32>(blended * src.a + dst.rgb * (1.0 - src.a), src.a + dst.a * (1.0 - src.a));
}
