// Video scopes: `accumulate` counts the pixels of the source into bins,
// `histogram_max` finds the tallest histogram bin, `draw` turns the bins
// into the scope's picture. Modes: 0 waveform, 1 RGB parade, 2 vectorscope,
// 3 histogram (see `ScopeKind` in scopes.rs).

struct Params {
    mode: u32,
    // Columns per waveform (per channel for the parade).
    bins_w: u32,
    src_w: u32,
    src_h: u32,
    // Every `step`-th pixel of the source is counted, on both axes.
    step: u32,
    out_w: u32,
    out_h: u32,
    _pad: u32,
    // Brightness per counted pixel of a trace (see `trace`).
    scale: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var output: texture_storage_2d<rgba8unorm, write>;

const LUMA_709: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);
const HISTOGRAM_MAX: u32 = 1024u;

fn level(v: f32) -> u32 {
    return u32(clamp(round(v * 255.0), 0.0, 255.0));
}

@compute @workgroup_size(16, 16)
fn accumulate(@builtin(global_invocation_id) id: vec3<u32>) {
    let p = id.xy * params.step;
    if (p.x >= params.src_w || p.y >= params.src_h) {
        return;
    }
    let c = textureLoad(source, p, 0).rgb;
    let luma = dot(c, LUMA_709);
    let col = p.x * params.bins_w / params.src_w;
    switch params.mode {
        case 0u: {
            atomicAdd(&bins[level(luma) * params.bins_w + col], 1u);
        }
        case 1u: {
            for (var k = 0u; k < 3u; k = k + 1u) {
                atomicAdd(&bins[(k * 256u + level(c[k])) * params.bins_w + col], 1u);
            }
        }
        case 2u: {
            let cb = level((c.b - luma) / 1.8556 + 0.5);
            let cr = level((c.r - luma) / 1.5748 + 0.5);
            atomicAdd(&bins[cr * 256u + cb], 1u);
        }
        default: {
            for (var k = 0u; k < 3u; k = k + 1u) {
                atomicAdd(&bins[k * 256u + level(c[k])], 1u);
            }
            atomicAdd(&bins[3u * 256u + level(luma)], 1u);
        }
    }
}

@compute @workgroup_size(256)
fn histogram_max(@builtin(local_invocation_index) i: u32) {
    for (var k = 0u; k < 4u; k = k + 1u) {
        atomicMax(&bins[HISTOGRAM_MAX], atomicLoad(&bins[k * 256u + i]));
    }
}

// A trace's brightness: dense where many pixels fall, never quite white.
fn trace(count: u32) -> f32 {
    return 1.0 - exp(-params.scale * f32(count));
}

// The brightest bin of a column among the levels the output pixel `y` spans
// (the output can be shorter than 256 levels).
fn column_peak(base: u32, col: u32, y: u32) -> u32 {
    let h = f32(params.out_h);
    let top = u32(clamp((1.0 - f32(y) / h) * 256.0, 0.0, 256.0));
    let bottom = u32(clamp((1.0 - f32(y + 1u) / h) * 256.0, 0.0, 255.0));
    var peak = 0u;
    for (var l = bottom; l < max(top, bottom + 1u); l = l + 1u) {
        peak = max(peak, atomicLoad(&bins[(base + l) * params.bins_w + col]));
    }
    return peak;
}

fn channel_color(k: u32) -> vec3<f32> {
    switch k {
        case 0u: { return vec3<f32>(1.0, 0.25, 0.25); }
        case 1u: { return vec3<f32>(0.3, 1.0, 0.35); }
        default: { return vec3<f32>(0.3, 0.45, 1.0); }
    }
}

@compute @workgroup_size(8, 8)
fn draw(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.out_w || id.y >= params.out_h) {
        return;
    }
    let fx = (f32(id.x) + 0.5) / f32(params.out_w);
    let fy = (f32(id.y) + 0.5) / f32(params.out_h);
    var color = vec3<f32>(0.0);
    switch params.mode {
        case 0u: {
            let col = min(u32(fx * f32(params.bins_w)), params.bins_w - 1u);
            color = vec3<f32>(0.6, 1.0, 0.65) * trace(column_peak(0u, col, id.y));
        }
        case 1u: {
            let panel = min(u32(fx * 3.0), 2u);
            let local = fx * 3.0 - f32(panel);
            let col = min(u32(local * f32(params.bins_w)), params.bins_w - 1u);
            color = channel_color(panel) * trace(column_peak(panel * 256u, col, id.y));
        }
        case 2u: {
            // A centred square, Cr up.
            let side = f32(min(params.out_w, params.out_h));
            let origin = (vec2<f32>(f32(params.out_w), f32(params.out_h)) - side) * 0.5;
            let q = (vec2<f32>(id.xy) + 0.5 - origin) / side;
            if (all(q >= vec2<f32>(0.0)) && all(q < vec2<f32>(1.0))) {
                let cb = q.x - 0.5;
                let cr = 0.5 - q.y;
                let count = atomicLoad(&bins[level(cr + 0.5) * 256u + level(cb + 0.5)]);
                let hue = clamp(
                    vec3<f32>(0.5 + 1.5748 * cr, 0.5 - 0.1873 * cb - 0.4681 * cr, 0.5 + 1.8556 * cb),
                    vec3<f32>(0.0),
                    vec3<f32>(1.0),
                );
                color = mix(vec3<f32>(1.0), hue, 0.6) * trace(count);
            }
        }
        default: {
            let l = min(u32(fx * 256.0), 255u);
            let peak = max(f32(atomicLoad(&bins[HISTOGRAM_MAX])), 1.0);
            let height = 1.0 - fy;
            for (var k = 0u; k < 3u; k = k + 1u) {
                if (height <= f32(atomicLoad(&bins[k * 256u + l])) / peak) {
                    color = color + channel_color(k) * 0.55;
                }
            }
            if (height <= f32(atomicLoad(&bins[3u * 256u + l])) / peak) {
                color = color + vec3<f32>(0.2);
            }
        }
    }
    textureStore(output, id.xy, vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0));
}
