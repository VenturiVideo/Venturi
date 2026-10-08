// Rec.709, prepended to the shaders that measure or convert color. Mirrors
// `vv_core::cb_cr` and `vv_core::chroma_shift`.

const LUMA_709: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, LUMA_709);
}

// Cb and Cr, each in -0.5..0.5.
fn cb_cr(c: vec3<f32>) -> vec2<f32> {
    let y = luma(c);
    return vec2<f32>((c.b - y) / 1.8556, (c.r - y) / 1.5748);
}

// The RGB adding `cb`/`cr` with no change in luma.
fn cb_cr_to_rgb(cb: f32, cr: f32) -> vec3<f32> {
    return vec3<f32>(1.5748 * cr, -0.187324 * cb - 0.468124 * cr, 1.8556 * cb);
}
