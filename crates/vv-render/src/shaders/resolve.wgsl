// The composed work texture (float) to the 8-bit output, dithered.

@group(0) @binding(0) var work_tex: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vertex_index << 1u) & 2u);
    let y = f32(vertex_index & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<u32>(position.xy);
    let c = clamp(textureLoad(work_tex, p, 0), vec4<f32>(0.0), vec4<f32>(1.0));
    let a = round(c.a * 255.0);
    let rgb = vec3<f32>(
        dither_round(c.r * 255.0, p),
        dither_round(c.g * 255.0, p),
        dither_round(c.b * 255.0, p),
    );
    // Premultiplied (transparent renders): the noise must not push the
    // color past the alpha.
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(a)), a) / 255.0;
}
