// Composed RGBA -> dense I420 (Y plane, then U, then V, no padding),
// BT.709 limited range. Each invocation writes 4 consecutive bytes of the
// flat buffer, so the rows do not have to be multiples of 4.

struct Params {
    width: u32,
    height: u32,
    chroma_width: u32,
    chroma_height: u32,
    // Invocations per row of the dispatch grid (2D past the 65535
    // workgroups per dimension).
    row_stride: u32,
    total_words: u32,
}

@group(0) @binding(0) var rgba: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> out_words: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// `i`, the byte index, seeds the dither: each byte gets its own noise.
fn to_byte(v: f32, i: u32) -> u32 {
    return u32(clamp(dither_round(v, vec2<u32>(i, 0u)), 0.0, 255.0));
}

fn pixel(x: i32, y: i32) -> vec3<f32> {
    let p = min(vec2<i32>(x, y), vec2<i32>(i32(params.width) - 1, i32(params.height) - 1));
    return textureLoad(rgba, p, 0).rgb;
}

fn byte_at(i: u32) -> u32 {
    let luma_len = params.width * params.height;
    if i < luma_len {
        return to_byte(16.0 + 219.0 * luma(pixel(i32(i % params.width), i32(i / params.width))), i);
    }
    let chroma_len = params.chroma_width * params.chroma_height;
    var j = i - luma_len;
    let is_v = j >= chroma_len;
    if is_v {
        j -= chroma_len;
    }
    if j >= chroma_len {
        return 0u;
    }
    let x = i32(j % params.chroma_width) * 2;
    let y = i32(j / params.chroma_width) * 2;
    let c = (pixel(x, y) + pixel(x + 1, y) + pixel(x, y + 1) + pixel(x + 1, y + 1)) * 0.25;
    let l = luma(c);
    if is_v {
        return to_byte(128.0 + 224.0 * (c.r - l) / 1.5748, i);
    }
    return to_byte(128.0 + 224.0 * (c.b - l) / 1.8556, i);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let word = id.y * params.row_stride + id.x;
    if word >= params.total_words {
        return;
    }
    let base = word * 4u;
    out_words[word] = byte_at(base)
        | (byte_at(base + 1u) << 8u)
        | (byte_at(base + 2u) << 16u)
        | (byte_at(base + 3u) << 24u);
}
