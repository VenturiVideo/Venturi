// Prepended to the shaders that write 8 bits (resolve.wgsl,
// rgba_to_i420.wgsl). Spatial noise only: the same frame always gives the
// same bytes, and the encoder sees no temporal flicker.

fn dither_hash(p: vec2<u32>) -> vec2<f32> {
    // pcg2d (Jarzynski & Olano, "Hash Functions for GPU Rendering").
    var v = p * 1664525u + 1013904223u;
    v.x += v.y * 1664525u;
    v.y += v.x * 1664525u;
    v = v ^ (v >> vec2<u32>(16u));
    v.x += v.y * 1664525u;
    v.y += v.x * 1664525u;
    v = v ^ (v >> vec2<u32>(16u));
    return vec2<f32>(v >> vec2<u32>(8u)) / 16777216.0;
}

// `v` in 8-bit steps (0-255) to the nearest step, with triangular noise of
// ±1 step. A value already on a step is left alone, so flat colors and
// black bars stay clean: f16 stores a step within 1/16 of it.
fn dither_round(v: f32, p: vec2<u32>) -> f32 {
    let step = round(v);
    if (abs(v - step) < 0.0625) {
        return step;
    }
    let h = dither_hash(p);
    return round(v + h.x + h.y - 1.0);
}
