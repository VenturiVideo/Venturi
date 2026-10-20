//! GPU compositing: planar YUV420 input converted to RGB in the shader,
//! layers composed in alpha-over from bottom to top.
//!
//! - `render_layers` / `render_layers_i420`: readback in RGBA or I420
//!   (export).
//! - `render_layers_to_texture`: stays on the GPU, for the preview that
//!   registers the texture in egui-wgpu. Requires `Compositor::new` on the
//!   same device as egui.

use std::sync::{Arc, Mutex};
use vv_core::{BlendMode, ColorMatrix, ProcessingPrecision, Transform};
use wgpu::util::DeviceExt;

/// What leaves the compositor (preview, readback): the work textures are
/// resolved to it with dithering.
const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// What the compositor composes into with `ProcessingPrecision::High`, so a
/// chain of passes does not round to 8 bits at every step. See `work_format`.
const PRECISE_WORK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const BLACK: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};
const TRANSPARENT: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.0,
};
const SOLID_PLACEHOLDER: YuvFrame<'static> = YuvFrame {
    y: &[0],
    width: 1,
    height: 1,
    chroma: YuvChroma::Planar {
        u: &[128],
        v: &[128],
    },
    chroma_width: 1,
    chroma_height: 1,
    matrix: ColorMatrix::Bt601,
    full_range: true,
    alpha: OPAQUE,
};
/// Placeholder for `YuvFrame::alpha` when the layer carries no real
/// per-pixel coverage: a single byte, sampled everywhere (`ClampToEdge`) —
/// zero cost for the common case (video/solid/text, always opaque).
const OPAQUE: &[u8] = &[255];
/// Format of the three input planes (Y/U/V): a single 8-bit channel, read
/// as `.r` in the shader.
const PLANE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
/// Interleaved U/V plane (NV12), read as `.rg` in the shader.
const UV_PLANE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg8Unorm;

/// Matrix selector for the shader: must stay aligned with
/// `kr_kb` in `transform.wgsl`.
fn shader_matrix_id(matrix: ColorMatrix) -> f32 {
    match matrix {
        ColorMatrix::Bt601 => 0.0,
        ColorMatrix::Bt709 => 1.0,
        ColorMatrix::Bt2020 => 2.0,
    }
}

/// 8-bit YUV420 frame with the color metadata. Dense planes, no padding.
#[derive(Clone, Copy)]
pub struct YuvFrame<'a> {
    pub y: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub chroma: YuvChroma<'a>,
    /// Chroma samples per row and rows (4:2:0 subsampled, typically
    /// `(width+1)/2` x `(height+1)/2` but not recomputed here: the
    /// caller passes the real dimensions allocated by the decoder).
    pub chroma_width: u32,
    pub chroma_height: u32,
    pub matrix: ColorMatrix,
    /// `true` = JPEG/full range (0-255), `false` = MPEG/limited range.
    pub full_range: bool,
    /// Per-pixel coverage, not subsampled: a single byte (`&[255]`,
    /// see `SOLID_PLACEHOLDER`) for "opaque everywhere" (a decoded file
    /// has no alpha channel), otherwise `width`x`height` bytes like Y — see
    /// `FrameYuv420::alpha`, where it comes from when it is not the placeholder.
    pub alpha: &'a [u8],
}

#[derive(Clone, Copy)]
pub enum YuvChroma<'a> {
    Planar {
        u: &'a [u8],
        v: &'a [u8],
    },
    /// NV12: U and V alternating, `2 * chroma_width` bytes a row.
    Interleaved(&'a [u8]),
}

/// A layer of the stack: what it shows and how it composes.
pub struct Layer<'a> {
    pub content: LayerContent<'a>,
    pub transform: Transform,
    /// Alpha multiplier of the whole layer (clip fades): 1.0 = no attenuation.
    pub opacity: f32,
    /// Active filters of the clip (`EffectStack::filters`), in the order
    /// they must be applied: vv-render does not know what each per-pixel one
    /// means, only the id of the shader corresponding to it
    /// (`filter_shader_id`). Blurs are passes of their own (`FilterChain`).
    pub filters: &'a [vv_core::FilterValue],
    /// How the layer composes onto those below.
    pub blend: BlendMode,
    /// Active masks of the clip (`EffectStack::masks`): where the layer
    /// shows, or for an adjustment where its processing does.
    pub masks: &'a [vv_core::MaskValue],
}

impl<'a> Layer<'a> {
    /// Opaque, no filters, no masks, `Normal` blend.
    pub fn new(content: LayerContent<'a>, transform: Transform) -> Self {
        Self {
            content,
            transform,
            opacity: 1.0,
            filters: &[],
            blend: BlendMode::Normal,
            masks: &[],
        }
    }
}

/// `Solid` and `Text` are treated as sources as large as the timeline: same
/// transform/crop.
#[derive(Clone, Copy)]
pub enum LayerContent<'a> {
    Video {
        frame: YuvFrame<'a>,
        /// *Native* resolution of the media, in which the `Transform`'s crop
        /// in pixels is expressed: not that of `frame`, which can be a
        /// reduced-resolution proxy.
        source_size: (u32, u32),
    },
    /// A frame already composed and resident on the GPU: the nested timeline of
    /// a compound clip, which becomes a layer again in the outer timeline without
    /// going through the CPU. Premultiplied RGBA — see `Fill::Rgba`.
    Texture {
        texture: &'a wgpu::Texture,
        /// As in `Video`: the units of the crop, which may not be the
        /// dimensions of `texture` (reduced-resolution preview).
        source_size: (u32, u32),
    },
    Solid(vv_core::Rgba),
    /// Title: rasterized at the output resolution (see `text`), then
    /// treated like a `Solid` as large as the timeline.
    Text(&'a vv_core::TitleParams),
    /// Adjustment clip: the stack composed so far, redrawn with the layer's
    /// transform and filters. It replaces the stack instead of going over
    /// it: where the transform or the crop leave the frame uncovered there is
    /// the clear color. Opacity mixes it with the original.
    Adjustment,
}

/// How many filters per layer the uniform can carry (see `filters` in
/// `TransformUniform`): past that, the excess filters are ignored. Generous
/// for real use, avoids a dynamically sized buffer for the shader.
const MAX_LAYER_FILTERS: usize = 8;

/// Shader id of each `FilterKind`; 0 is reserved for "empty slot". The
/// blurs never reach the uniform: see `FilterChain`.
fn filter_shader_id(kind: vv_core::FilterKind) -> f32 {
    match kind {
        vv_core::FilterKind::ColorCorrection => 1.0,
        vv_core::FilterKind::Exposure => 2.0,
        vv_core::FilterKind::BoxBlur
        | vv_core::FilterKind::GaussianBlur
        | vv_core::FilterKind::DropShadow => 0.0,
    }
}

/// Past that many taps per side a blur samples every few texels instead of
/// every one: the cost stays bounded at large radii.
const MAX_BLUR_TAPS: f32 = 96.0;

/// Past that reach a neighbourhood effect halves its texture first (see
/// `Compositor::reduced`): one that wide loses nothing at the lower
/// resolution, and costs a fraction.
const MAX_REACH_TEXELS: f32 = 32.0;

/// Squared distances of `Compositor::grown`, past what 8 bits can hold.
const DISTANCE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct GrowUniform {
    params: [f32; 4],
}

/// The texture where a layer's neighbourhood effects (blurs, drop shadow)
/// run. Their sizes are in timeline pixels and become texels only through
/// `texels`: how many texels to spend is decided by `of_source`,
/// `of_output` and `Compositor::reduced`, never by the effect.
#[derive(Debug, Clone, Copy, PartialEq)]
struct WorkSpace {
    size: [u32; 2],
    texels_per_pixel: [f32; 2],
}

impl WorkSpace {
    fn of_output(output: OutputFrame) -> Self {
        Self {
            size: [output.width, output.height],
            texels_per_pixel: [
                output.width as f32 / output.timeline_size.0.max(1) as f32,
                output.height as f32 / output.timeline_size.1.max(1) as f32,
            ],
        }
    }

    /// A source of `texture_size` texels drawn with the identity transform.
    fn of_source(texture_size: (u32, u32), transform: &Transform, output: OutputFrame) -> Self {
        let (sw, sh) = (texture_size.0.max(1) as f32, texture_size.1.max(1) as f32);
        // Output pixels per source texel at zoom 1.
        let fit_scale = (output.width as f32 / sw).min(output.height as f32 / sh);
        let zoom = transform.zoom[0]
            .abs()
            .max(transform.zoom[1].abs())
            .max(1.0);
        // No more texels than the output shows.
        let scale = (fit_scale * zoom).min(1.0);
        let texels_per_pixel =
            output.width as f32 / output.timeline_size.0.max(1) as f32 / fit_scale * scale;
        Self {
            size: [
                ((sw * scale).round() as u32).max(1),
                ((sh * scale).round() as u32).max(1),
            ],
            texels_per_pixel: [texels_per_pixel; 2],
        }
    }

    fn texels(&self, timeline_pixels: f32, axis: usize) -> f32 {
        timeline_pixels * self.texels_per_pixel[axis]
    }
}

/// One encoder being recorded, with the textures its passes read: they go
/// back to the pools only once it is submitted.
struct Recording {
    encoder: wgpu::CommandEncoder,
    planes: Vec<wgpu::Texture>,
    scratch: Vec<Option<wgpu::Texture>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Blur {
    gaussian: bool,
    /// In timeline pixels (`ClipFilter::radius`).
    radius: f32,
    direction: vv_core::BlurDirection,
}

/// A layer's filters split at the blurs: those need neighbouring pixels,
/// so they cannot run inside the transform shader like the per-pixel ones.
#[derive(Debug, Default, PartialEq)]
struct FilterChain {
    /// Per-pixel filters before the first blur.
    leading: Vec<vv_core::FilterValue>,
    /// Each blur with the per-pixel filters following it.
    blurs: Vec<(Blur, Vec<vv_core::FilterValue>)>,
    /// Cast by the layer as it ends up, wherever it is in the list.
    shadow: Option<vv_core::FilterValue>,
}

impl FilterChain {
    /// `uniform`: the layer is a single color, where a blur changes nothing.
    fn new(filters: &[vv_core::FilterValue], uniform: bool) -> Self {
        let mut chain = Self::default();
        for filter in filters {
            if filter.kind == vv_core::FilterKind::DropShadow {
                chain.shadow = Some(*filter);
            } else if filter.kind.is_blur() {
                if filter.radius > 0.0 && !uniform {
                    let blur = Blur {
                        gaussian: filter.kind == vv_core::FilterKind::GaussianBlur,
                        radius: filter.radius,
                        direction: filter.direction,
                    };
                    chain.blurs.push((blur, Vec::new()));
                }
            } else {
                match chain.blurs.last_mut() {
                    Some((_, run)) => run.push(*filter),
                    None => chain.leading.push(*filter),
                }
            }
        }
        chain
    }
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowUniform {
    color: [f32; 4],
    offset: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniform {
    step_taps: [f32; 4],
    shape: [f32; 4],
}

impl BlurUniform {
    /// No taps: a single bilinear sample, which halving a texture turns into
    /// the average of the texels it covers.
    const RESAMPLE: Self = Self {
        step_taps: [0.0; 4],
        shape: [0.0; 4],
    };

    /// `radius` in texels of a `size` texture, along `axis` (0 = x, 1 = y).
    fn new(blur: Blur, radius: f32, size: (u32, u32), axis: usize) -> Self {
        let stride = (radius / MAX_BLUR_TAPS).max(1.0);
        let radius = radius / stride;
        let taps = if radius > 0.0 { radius.ceil() } else { 0.0 };
        let mut step = [0.0; 2];
        step[axis] = stride / [size.0, size.1][axis].max(1) as f32;
        Self {
            step_taps: [
                step[0],
                step[1],
                taps,
                if blur.gaussian { 1.0 } else { 0.0 },
            ],
            // The kernel is cut at 3 sigma.
            shape: [radius, radius / 3.0, 0.0, 0.0],
        }
    }
}

/// Shader id of each `BlendMode`, aligned with the `switch` of
/// `blend_channel` in `transform.wgsl`; 0 = Normal, the only one using
/// the pipeline's alpha blending instead of reading the backdrop.
fn blend_shader_id(mode: BlendMode) -> f32 {
    match mode {
        BlendMode::Normal => 0.0,
        BlendMode::Add => 1.0,
        BlendMode::Multiply => 2.0,
        BlendMode::Screen => 3.0,
        BlendMode::Overlay => 4.0,
        BlendMode::Darken => 5.0,
        BlendMode::Lighten => 6.0,
        BlendMode::ColorDodge => 7.0,
        BlendMode::ColorBurn => 8.0,
        BlendMode::HardLight => 9.0,
        BlendMode::SoftLight => 10.0,
        BlendMode::Difference => 11.0,
        BlendMode::Exclusion => 12.0,
        BlendMode::Subtract => 13.0,
        BlendMode::Divide => 14.0,
    }
}

/// What colors a layer: the Y/U/V planes, a solid color, or a solid
/// color with the coverage taken from the Y plane.
#[derive(Clone, Copy)]
enum Fill {
    Video,
    Solid(vv_core::Rgba),
    Mask(vv_core::Rgba),
    /// Already composed RGBA texture, with the color premultiplied by the alpha
    /// (it is the result of an `ALPHA_BLENDING` onto a transparent clear): the
    /// shader divides it out before putting it back into alpha-over.
    Rgba,
}

/// Pixel resolution of the produced texture and the logical one of the
/// timeline, in which position and anchor are expressed. They coincide
/// on export; the preview composes at the resolution of the decoded frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputFrame {
    pub width: u32,
    pub height: u32,
    pub timeline_size: (u32, u32),
}

impl OutputFrame {
    /// Output at the timeline resolution.
    pub fn exact(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            timeline_size: (width, height),
        }
    }

    /// Output at a resolution different from the timeline's, with the
    /// same aspect ratio.
    pub fn scaled(width: u32, height: u32, timeline_size: (u32, u32)) -> Self {
        Self {
            width,
            height,
            timeline_size,
        }
    }

    /// No larger than the timeline divided by `divisor`, same aspect: a
    /// cheaper preview on a weak GPU.
    pub fn reduced(self, divisor: u32) -> Self {
        let divisor = divisor.max(1);
        let (max_w, max_h) = (
            (self.timeline_size.0 / divisor).max(1),
            (self.timeline_size.1 / divisor).max(1),
        );
        let factor =
            (max_w as f64 / self.width.max(1) as f64).min(max_h as f64 / self.height.max(1) as f64);
        if factor >= 1.0 {
            return self;
        }
        Self {
            width: ((self.width as f64 * factor).round() as u32).max(1),
            height: ((self.height as f64 * factor).round() as u32).max(1),
            ..self
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct TransformUniform {
    crop: [f32; 4],
    zoom_pos: [f32; 4],
    fit_rot: [f32; 4],
    anchor_flip: [f32; 4],
    color: [f32; 4],
    solid: [f32; 4],
    /// x: opacity of the whole layer. y: id of the compositing method
    /// (`blend_shader_id`). z: opacity of an adjustment layer, w: 1 if it is one.
    extra: [f32; 4],
    /// x: 1 if U/V are interleaved in the U texture (NV12).
    planes: [f32; 4],
    /// Shader ids of the active filters, in order of application (see
    /// `filter_shader_id`); 0 = empty slot. `MAX_LAYER_FILTERS` in two vec4s
    /// for the uniform alignment.
    filters: [[f32; 4]; MAX_LAYER_FILTERS / 4],
    /// The scalar parameter of each slot of `filters` (`FilterValue::amount`).
    filter_params: [[f32; 4]; MAX_LAYER_FILTERS / 4],
}

impl TransformUniform {
    fn new(
        t: &Transform,
        matrix: ColorMatrix,
        full_range: bool,
        fit: [f32; 2],
        output: OutputFrame,
        source_size: (u32, u32),
        fill: Fill,
        opacity: f32,
        filters: &[vv_core::FilterValue],
        blend: BlendMode,
    ) -> Self {
        let (mode, solid) = match fill {
            Fill::Video => (0.0, None),
            Fill::Solid(c) => (1.0, Some(c)),
            Fill::Mask(c) => (2.0, Some(c)),
            Fill::Rgba => (3.0, None),
        };
        // The `Transform` is in pixels — of the timeline for position and anchor,
        // of the media for the crop; the shader works in normalized
        // coordinates.
        let (frame_w, frame_h) = (
            output.timeline_size.0.max(1) as f32,
            output.timeline_size.1.max(1) as f32,
        );
        let (source_w, source_h) = (source_size.0.max(1) as f32, source_size.1.max(1) as f32);
        Self {
            // From the per-side cuts to the rectangle the shader samples.
            crop: [
                t.crop[0] / source_w,
                t.crop[1] / source_h,
                1.0 - t.crop[2] / source_w,
                1.0 - t.crop[3] / source_h,
            ],
            // The model's Y axis points up (as in an NLE), the uv one
            // points down.
            zoom_pos: [
                t.zoom[0],
                t.zoom[1],
                t.position[0] / frame_w,
                -t.position[1] / frame_h,
            ],
            fit_rot: [
                fit[0],
                fit[1],
                t.rotation.to_radians(),
                // The softness follows the crop: media pixels, and on the
                // shorter axis, so it stays isotropic.
                t.crop_softness / source_w.min(source_h),
            ],
            anchor_flip: [
                t.anchor[0] / frame_w,
                -t.anchor[1] / frame_h,
                if t.flip[0] { 1.0 } else { 0.0 },
                if t.flip[1] { 1.0 } else { 0.0 },
            ],
            color: [
                shader_matrix_id(matrix),
                if full_range { 1.0 } else { 0.0 },
                output.width as f32 / output.height.max(1) as f32,
                mode,
            ],
            solid: solid.map_or([0.0; 4], |c| {
                [
                    c.r.clamp(0.0, 1.0),
                    c.g.clamp(0.0, 1.0),
                    c.b.clamp(0.0, 1.0),
                    c.a.clamp(0.0, 1.0),
                ]
            }),
            extra: [opacity.clamp(0.0, 1.0), blend_shader_id(blend), 0.0, 0.0],
            planes: [0.0; 4],
            filters: pack_filter_slots(filters, |f| filter_shader_id(f.kind)),
            filter_params: pack_filter_slots(filters, |f| f.amount),
        }
    }
}

fn pack_filter_slots(
    filters: &[vv_core::FilterValue],
    value: impl Fn(&vv_core::FilterValue) -> f32,
) -> [[f32; 4]; MAX_LAYER_FILTERS / 4] {
    let mut slots = [0.0f32; MAX_LAYER_FILTERS];
    for (slot, filter) in slots.iter_mut().zip(filters.iter().take(MAX_LAYER_FILTERS)) {
        *slot = value(filter);
    }
    [
        [slots[0], slots[1], slots[2], slots[3]],
        [slots[4], slots[5], slots[6], slots[7]],
    ]
}

/// Past these, the excess masks and polygon points are dropped; sized for a
/// uniform buffer, which (unlike a storage one) every backend has.
const MAX_MASKS: usize = 8;
const MAX_MASK_POINTS: usize = 256;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct MaskData {
    /// x: shape (0 rectangle, 1 ellipse, 2 polygon), y: mode (0 add,
    /// 1 subtract, 2 intersect), z: 1 if inverted, w: opacity 0-1.
    shape_mode: [f32; 4],
    /// Center and half size, layer pixels, Y up.
    center_size: [f32; 4],
    /// Rotation (radians, clockwise), corner radius, feather, expansion.
    params: [f32; 4],
    /// x: first point in `MaskUniform::points`, y: point count.
    points: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct MaskUniform {
    /// x: mask count, y: layer pixels per output pixel (antialiasing width),
    /// zw: timeline size.
    header: [f32; 4],
    masks: [MaskData; MAX_MASKS],
    /// Two points per vec4.
    points: [[f32; 4]; MAX_MASK_POINTS / 2],
}

impl MaskUniform {
    fn new(masks: &[vv_core::MaskValue], transform: &Transform, output: OutputFrame) -> Self {
        let mut uniform = <Self as bytemuck::Zeroable>::zeroed();
        let zoom = transform.zoom[0]
            .abs()
            .max(transform.zoom[1].abs())
            .max(1e-4);
        uniform.header = [
            masks.len().min(MAX_MASKS) as f32,
            output.timeline_size.0 as f32 / output.width.max(1) as f32 / zoom,
            output.timeline_size.0 as f32,
            output.timeline_size.1 as f32,
        ];
        let mut point_len = 0usize;
        for (data, mask) in uniform.masks.iter_mut().zip(masks.iter().take(MAX_MASKS)) {
            let first = point_len;
            let budget = MAX_MASK_POINTS - first;
            // Decimated evenly rather than cut, so the shape stays closed.
            let step = mask.polygon.len().div_ceil(budget.max(1)).max(1);
            for &pt in mask.polygon.iter().step_by(step).take(budget) {
                let slot = &mut uniform.points[point_len / 2];
                let base = (point_len % 2) * 2;
                slot[base] = pt[0];
                slot[base + 1] = pt[1];
                point_len += 1;
            }
            data.shape_mode = [
                match mask.shape {
                    vv_core::MaskShape::Rectangle => 0.0,
                    vv_core::MaskShape::Ellipse => 1.0,
                    vv_core::MaskShape::Path => 2.0,
                },
                match mask.mode {
                    vv_core::MaskMode::Add => 0.0,
                    vv_core::MaskMode::Subtract => 1.0,
                    vv_core::MaskMode::Intersect => 2.0,
                },
                if mask.invert { 1.0 } else { 0.0 },
                mask.opacity,
            ];
            data.center_size = [
                mask.center[0],
                mask.center[1],
                mask.size[0] / 2.0,
                mask.size[1] / 2.0,
            ];
            data.params = [
                mask.rotation.to_radians(),
                mask.roundness,
                mask.feather,
                mask.expansion,
            ];
            data.points = [first as f32, (point_len - first) as f32, 0.0, 0.0];
        }
        uniform
    }
}

/// The pipelines writing the work texture, for one work format.
struct WorkPipelines {
    format: wgpu::TextureFormat,
    normal: wgpu::RenderPipeline,
    /// Like `normal`, but in REPLACE: used by the compositing methods
    /// other than Normal (see `blend_shader_id`).
    blend: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    shadow: wgpu::RenderPipeline,
    /// The two passes of `Compositor::grown`.
    grow_rows: wgpu::RenderPipeline,
    grow: wgpu::RenderPipeline,
    matte: wgpu::RenderPipeline,
}

impl WorkPipelines {
    fn new(
        device: &wgpu::Device,
        bind_group_layout: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vv-render transform shader"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("shaders/color.wgsl"),
                    include_str!("shaders/transform.wgsl")
                )
                .into(),
            ),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vv-render transform pipeline layout"),
            bind_group_layouts: &[Some(bind_group_layout)],
            immediate_size: 0,
        });

        let transform_pipeline = |label, blend, format| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        // Normal: not REPLACE, because the uncovered areas (letterbox) come out with
        // alpha 0 and must show the layer below. The other compositing
        // methods read the layer below themselves (`backdrop_tex`) and
        // write the already composed result.
        let normal = transform_pipeline(
            "vv-render transform pipeline",
            Some(wgpu::BlendState::ALPHA_BLENDING),
            format,
        );
        let blend = transform_pipeline(
            "vv-render blend pipeline",
            Some(wgpu::BlendState::REPLACE),
            format,
        );

        let module = |label, source: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            })
        };
        let fullscreen_pipeline = |label, shader: &wgpu::ShaderModule, entry, format, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: None,
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: shader,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let blur = fullscreen_pipeline(
            "vv-render blur pipeline",
            &module("vv-render blur shader", include_str!("shaders/blur.wgsl")),
            "fs_main",
            format,
            wgpu::BlendState::REPLACE,
        );
        let shadow = fullscreen_pipeline(
            "vv-render shadow pipeline",
            &module(
                "vv-render shadow shader",
                include_str!("shaders/shadow.wgsl"),
            ),
            "fs_main",
            format,
            wgpu::BlendState::ALPHA_BLENDING,
        );
        let grow_shader = module("vv-render grow shader", include_str!("shaders/grow.wgsl"));
        let grow_rows = fullscreen_pipeline(
            "vv-render grow rows pipeline",
            &grow_shader,
            "fs_rows",
            DISTANCE_FORMAT,
            wgpu::BlendState::REPLACE,
        );
        let grow = fullscreen_pipeline(
            "vv-render grow pipeline",
            &grow_shader,
            "fs_grow",
            format,
            wgpu::BlendState::REPLACE,
        );

        // Multiplies the target, color and alpha, by the coverage the shader writes.
        let by_coverage = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::SrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let matte = fullscreen_pipeline(
            "vv-render matte pipeline",
            &module("vv-render matte shader", include_str!("shaders/matte.wgsl")),
            "fs_main",
            format,
            wgpu::BlendState {
                color: by_coverage,
                alpha: by_coverage,
            },
        );

        Self {
            format,
            normal,
            blend,
            blur,
            shadow,
            grow_rows,
            grow,
            matte,
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct GradeUniform {
    /// Shadows, midtones, highlights, offset: RGB shift and saturation.
    wheels: [[f32; 4]; 4],
    /// x: shadows/midtones luma, y: midtones/highlights luma.
    ranges: [f32; 4],
}

impl GradeUniform {
    /// From the pass's color correction; a pass has at most one, since a clip
    /// has at most one filter per kind.
    fn new(filters: &[vv_core::FilterValue]) -> Self {
        let grade = filters
            .iter()
            .find(|f| f.kind == vv_core::FilterKind::ColorCorrection)
            .map_or(vv_core::GradeValue::NEUTRAL, |f| f.grade);
        let mut wheels = [[0.0; 4]; 4];
        for (slot, wheel) in wheels.iter_mut().zip(vv_core::GradeWheel::ALL) {
            let [r, g, b] = grade.wheel_shift(wheel);
            *slot = [r, g, b, grade.get(wheel.saturation())];
        }
        Self {
            wheels,
            ranges: [
                grade.get(vv_core::GradeParam::LowRange),
                grade.get(vv_core::GradeParam::HighRange),
                0.0,
                0.0,
            ],
        }
    }
}

pub struct Compositor {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    work: WorkPipelines,
    precision: ProcessingPrecision,
    /// Whether the adapter can compose in `PRECISE_WORK_FORMAT`.
    float_work: bool,
    resolve_pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    i420_pipeline: wgpu::ComputePipeline,
    zero_view: wgpu::TextureView,
    neutral_view: wgpu::TextureView,
    opaque_view: wgpu::TextureView,
    empty_mask_buffer: wgpu::Buffer,
    neutral_grade_buffer: wgpu::Buffer,
    /// Textures reused from one frame to the next, by size: allocating new
    /// ones on every frame costs more than the drawing itself.
    pool: Mutex<TexturePool>,
    /// A separate pool for the work textures (see `PooledTexture`): they go back
    /// there when whoever uses them lets them go, not at the end of the render.
    scratch: Arc<Mutex<Vec<wgpu::Texture>>>,
    /// Name and backend.
    adapter_name: String,
    /// Built on first use: most projects never need it.
    smooth_cut: std::sync::OnceLock<crate::smooth_cut::SmoothCut>,
}

#[derive(Default)]
struct TexturePool {
    planes: Vec<wgpu::Texture>,
    /// Resolved, in `OUTPUT_FORMAT`.
    outputs: Vec<wgpu::Texture>,
    i420: Option<I420Buffers>,
    /// Uploaded masks of recent titles, keyed by `TitleRender::id`.
    titles: Vec<(u64, Vec<CachedTitleMask>)>,
}

type CachedTitleMask = (wgpu::Texture, wgpu::TextureView, vv_core::Rgba);

/// Buffer of the I420 conversion, for the size of the last frame.
struct I420Buffers {
    size: wgpu::BufferAddress,
    storage: wgpu::Buffer,
    params: wgpu::Buffer,
    readback: wgpu::Buffer,
}

/// An intermediate texture (the composed frame of a nested timeline) that
/// returns to the pool by itself: while someone holds it as a layer no other
/// render draws over it, and when they let it go it is available
/// again, without reallocating it on every frame.
pub struct PooledTexture {
    texture: Option<wgpu::Texture>,
    pool: Arc<Mutex<Vec<wgpu::Texture>>>,
}

impl std::ops::Deref for PooledTexture {
    type Target = wgpu::Texture;

    fn deref(&self) -> &wgpu::Texture {
        self.texture
            .as_ref()
            .expect("the texture exists until Drop")
    }
}

impl Drop for PooledTexture {
    fn drop(&mut self) {
        if let Some(texture) = self.texture.take() {
            give_back(&mut self.pool.lock().unwrap(), [texture]);
        }
    }
}

/// Past that, the textures of no longer used sizes are let go.
const MAX_POOLED: usize = 32;

fn take_sized(
    pool: &mut Vec<wgpu::Texture>,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> Option<wgpu::Texture> {
    let i = pool
        .iter()
        .position(|t| t.width() == width && t.height() == height && t.format() == format)?;
    Some(pool.swap_remove(i))
}

fn give_back(pool: &mut Vec<wgpu::Texture>, textures: impl IntoIterator<Item = wgpu::Texture>) {
    pool.extend(textures);
    let excess = pool.len().saturating_sub(MAX_POOLED);
    pool.drain(..excess);
}

/// Whether the adapter can render, blend and filter `PRECISE_WORK_FORMAT`
/// (`features` are its own).
fn float_work_supported(features: wgpu::TextureFormatFeatures) -> bool {
    let needed =
        wgpu::TextureFormatFeatureFlags::BLENDABLE | wgpu::TextureFormatFeatureFlags::FILTERABLE;
    features
        .allowed_usages
        .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        && features.flags.contains(needed)
}

/// Without float support, `High` falls back to the 8-bit output format.
fn work_format(precision: ProcessingPrecision, float_work: bool) -> wgpu::TextureFormat {
    match precision {
        ProcessingPrecision::High if float_work => PRECISE_WORK_FORMAT,
        _ => OUTPUT_FORMAT,
    }
}

/// wgpu refuses a device asking for more than the adapter offers, and the
/// defaults exceed small GPUs (Raspberry Pi 4: 4 color attachments, 4096
/// textures on GL).
pub fn device_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    wgpu::Limits {
        max_texture_dimension_2d: 8192,
        ..wgpu::Limits::default()
    }
    .or_worse_values_from(&adapter.limits())
}

impl Compositor {
    /// `adapter`: the one `device` comes from, asked which work format it
    /// supports.
    pub fn new(
        adapter: &wgpu::Adapter,
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        precision: ProcessingPrecision,
    ) -> Self {
        let float_work =
            float_work_supported(adapter.get_texture_format_features(PRECISE_WORK_FORMAT));
        let info = adapter.get_info();
        let adapter_name = format!("{} ({:?})", info.name, info.backend);
        if !float_work {
            eprintln!("vv-render: {adapter_name} cannot compose in {PRECISE_WORK_FORMAT:?}");
        }

        // Three input textures (Y/U/V, bindings 0-2) instead of a single
        // RGBA one: the YUV→RGB conversion happens in the shader
        // (plans/REFACTOR_PIPELINE.md B3), only the raw planes arrive here.
        let plane_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vv-render transform bind group layout"),
            entries: &[
                plane_entry(0), // Y
                plane_entry(1), // U
                plane_entry(2), // V
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                plane_entry(5), // Alpha (per-pixel coverage, see YuvFrame::alpha)
                plane_entry(6), // Backdrop (see `backdrop_tex` in the shader)
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let work = WorkPipelines::new(
            &device,
            &bind_group_layout,
            work_format(precision, float_work),
        );

        let resolve_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vv-render resolve shader"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("shaders/dither.wgsl"),
                    include_str!("shaders/resolve.wgsl")
                )
                .into(),
            ),
        });
        let resolve_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vv-render resolve pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &resolve_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &resolve_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: OUTPUT_FORMAT,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vv-render transform sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let i420_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vv-render rgba->i420 shader"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("shaders/color.wgsl"),
                    include_str!("shaders/dither.wgsl"),
                    include_str!("shaders/rgba_to_i420.wgsl")
                )
                .into(),
            ),
        });
        let i420_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("vv-render rgba->i420 pipeline"),
            layout: None,
            module: &i420_shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        let const_plane = |label, byte: u8| {
            let texture = device.create_texture_with_data(
                &queue,
                &wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: PLANE_FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &[byte],
            );
            texture.create_view(&wgpu::TextureViewDescriptor::default())
        };
        let zero_view = const_plane("vv-render zero plane", 0);
        let neutral_view = const_plane("vv-render neutral plane", 128);
        let opaque_view = const_plane("vv-render opaque plane", 255);
        let empty_mask_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vv-render empty mask uniform"),
            contents: bytemuck::bytes_of(&<MaskUniform as bytemuck::Zeroable>::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let neutral_grade_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vv-render neutral grade uniform"),
            contents: bytemuck::bytes_of(&GradeUniform::new(&[])),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let compositor = Self {
            device,
            queue,
            work,
            precision,
            float_work,
            resolve_pipeline,
            bind_group_layout,
            sampler,
            i420_pipeline,
            zero_view,
            neutral_view,
            opaque_view,
            empty_mask_buffer,
            neutral_grade_buffer,
            pool: Mutex::default(),
            scratch: Arc::default(),
            adapter_name,
            smooth_cut: std::sync::OnceLock::new(),
        };
        compositor.log_work_format();
        compositor
    }

    /// The device and queue the compositor's textures live on: a `Scopes`
    /// measuring them must use the same.
    pub fn device(&self) -> &Arc<wgpu::Device> {
        &self.device
    }

    pub fn queue(&self) -> &Arc<wgpu::Queue> {
        &self.queue
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    pub fn precision(&self) -> ProcessingPrecision {
        self.precision
    }

    /// Whether `ProcessingPrecision::High` really composes in float here.
    pub fn float_work_supported(&self) -> bool {
        self.float_work
    }

    /// GPU memory of the textures kept for reuse, i.e. the working set of
    /// the frames rendered so far; owned textures still held are not counted.
    pub fn pooled_texture_bytes(&self) -> u64 {
        let bytes = |t: &wgpu::Texture| {
            let block = t.format().block_copy_size(None).unwrap_or(4) as u64;
            t.width() as u64 * t.height() as u64 * block
        };
        let pool = self.pool.lock().unwrap();
        let scratch = self.scratch.lock().unwrap();
        pool.planes
            .iter()
            .chain(&pool.outputs)
            .chain(
                pool.titles
                    .iter()
                    .flat_map(|(_, layers)| layers.iter().map(|(t, _, _)| t)),
            )
            .chain(scratch.iter())
            .map(bytes)
            .sum()
    }

    /// The textures already handed out keep their format: an owned one
    /// still works as a `LayerContent::Texture`.
    pub fn set_precision(&mut self, precision: ProcessingPrecision) {
        if precision == self.precision {
            return;
        }
        self.precision = precision;
        let format = work_format(precision, self.float_work);
        if format != self.work.format {
            self.work = WorkPipelines::new(&self.device, &self.bind_group_layout, format);
            self.scratch.lock().unwrap().clear();
        }
        self.log_work_format();
    }

    /// For the manual tests: which precision is actually in use, and where.
    fn log_work_format(&self) {
        eprintln!(
            "vv-render: {:?} precision, composing in {:?} on {}",
            self.precision, self.work.format, self.adapter_name
        );
    }

    pub fn new_headless() -> Self {
        Self::new_headless_with_precision(ProcessingPrecision::default())
    }

    /// Creates an independent wgpu device (headless, no surface) to
    /// use the compositor outside an eframe/egui-wgpu context — useful
    /// for the app today and for the tests.
    pub fn new_headless_with_precision(precision: ProcessingPrecision) -> Self {
        let (adapter, (device, queue)) = pollster::block_on(async {
            let instance = wgpu::Instance::default();
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
                .expect("no wgpu adapter available");
            let device = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("vv-render headless device"),
                    required_limits: device_limits(&adapter),
                    ..Default::default()
                })
                .await
                .expect("wgpu device request failed");
            (adapter, device)
        });
        Self::new(&adapter, Arc::new(device), Arc::new(queue), precision)
    }

    /// Like `render_layers`, but dense I420 BT.709 limited: doing the conversion on
    /// the GPU saves doing it on the CPU and halves the readback.
    pub fn render_layers_i420(&self, layers: &[Layer], output: OutputFrame) -> Vec<u8> {
        const WORKGROUP: u32 = 256;
        const MAX_GROUPS_PER_DIM: u32 = 65535;

        // Read in float, not resolved: the conversion dithers once, in YUV.
        let work = self.compose(layers, output, BLACK);
        let (w, h) = (output.width, output.height);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let len = (w * h + 2 * cw * ch) as usize;
        let total_words = len.div_ceil(4) as u32;
        let groups = total_words.div_ceil(WORKGROUP);
        let groups_x = groups.min(MAX_GROUPS_PER_DIM);
        let groups_y = groups.div_ceil(groups_x);
        let params: [u32; 8] = [w, h, cw, ch, groups_x * WORKGROUP, total_words, 0, 0];

        let buffer_size = total_words as wgpu::BufferAddress * 4;
        let i420 = self
            .pool
            .lock()
            .unwrap()
            .i420
            .take()
            .filter(|b| b.size == buffer_size)
            .unwrap_or_else(|| I420Buffers {
                size: buffer_size,
                storage: self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("vv-render i420 storage"),
                    size: buffer_size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                params: self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("vv-render i420 params"),
                    size: std::mem::size_of_val(&params) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                readback: self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("vv-render i420 readback"),
                    size: buffer_size,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
            });
        self.queue
            .write_buffer(&i420.params, 0, bytemuck::cast_slice(&params));
        let texture_view = work.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vv-render i420 bind group"),
            layout: &self.i420_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: i420.storage.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: i420.params.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vv-render i420 encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vv-render i420 pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.i420_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(groups_x, groups_y, 1);
        }
        encoder.copy_buffer_to_buffer(&i420.storage, 0, &i420.readback, 0, buffer_size);
        self.queue.submit(Some(encoder.finish()));
        give_back(&mut self.scratch.lock().unwrap(), [work]);

        let out = self.map_read(&i420.readback, |data| data[..len].to_vec());
        self.pool.lock().unwrap().i420 = Some(i420);
        out
    }

    /// Like `render_layers_i420`, but RGBA8 with a transparent background
    /// instead of opaque black and without conversion to YUV: used to compose
    /// the nested timeline of a compound clip, whose result becomes
    /// a layer elsewhere in turn — the real alpha must be preserved, `_i420`
    /// would lose it (I420 has no alpha channel).
    pub fn render_layers_rgba_transparent(&self, layers: &[Layer], output: OutputFrame) -> Vec<u8> {
        let output_texture = self.render_layers_to_texture_transparent(layers, output);
        self.read_rgba_texture(&output_texture, output.width, output.height)
    }

    /// Reads an RGBA8 texture (`OUTPUT_FORMAT`) into a dense `Vec<u8>`,
    /// removing the row padding `wgpu` requires on the destination
    /// buffer.
    pub fn read_rgba_texture(&self, texture: &wgpu::Texture, width: u32, height: u32) -> Vec<u8> {
        let unpadded_bytes_per_row = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;

        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vv-render readback buffer"),
            size: (padded_bytes_per_row * height) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vv-render readback encoder"),
            });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &output_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        self.map_read(&output_buffer, |data| {
            let mut out = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
            for row in 0..height {
                let start = (row * padded_bytes_per_row) as usize;
                out.extend_from_slice(&data[start..start + unpadded_bytes_per_row as usize]);
            }
            out
        })
    }

    /// Multi-layer version of [`Compositor::render_frame_to_texture`]
    /// (see [`Compositor::render_layers`]). Opaque black background: for the
    /// final video (preview, export) there is no "transparent".
    pub fn render_layers_to_texture(&self, layers: &[Layer], output: OutputFrame) -> wgpu::Texture {
        self.render_resolved(layers, output, BLACK)
    }

    /// Like `render_layers_to_texture`, but without forcing an opaque background:
    /// used to compose the nested timeline of a compound clip, whose
    /// result becomes a layer elsewhere in turn — the areas where that
    /// timeline has nothing to show must stay transparent, not
    /// black, or they would cover what is below instead of letting it show
    /// (see `YuvFrame::alpha`, which carries this transparency around).
    pub fn render_layers_to_texture_transparent(
        &self,
        layers: &[Layer],
        output: OutputFrame,
    ) -> wgpu::Texture {
        self.render_resolved(layers, output, TRANSPARENT)
    }

    /// Like `render_layers_to_texture_transparent`, but the texture stays with
    /// whoever receives it until they let it go, so it can be used in the meantime
    /// as a `LayerContent::Texture`: with immediate recycling the first render of the
    /// same size would draw over it. Not resolved: the work format keeps its
    /// precision into the timeline that uses it.
    pub fn render_layers_to_owned_texture_transparent(
        &self,
        layers: &[Layer],
        output: OutputFrame,
    ) -> PooledTexture {
        PooledTexture {
            texture: Some(self.compose(layers, output, TRANSPARENT)),
            pool: Arc::clone(&self.scratch),
        }
    }

    /// The frame at `progress` (0..1) of a Smooth Cut from `from` to `to`:
    /// each side composed alone, then morphed along their optical flow.
    /// Owned like `render_layers_to_owned_texture_transparent`, to be used as a
    /// `LayerContent::Texture`.
    pub fn render_smooth_cut_to_owned_texture(
        &self,
        from: &Layer,
        to: &Layer,
        progress: f32,
        output: OutputFrame,
    ) -> PooledTexture {
        let a = self.compose(std::slice::from_ref(from), output, TRANSPARENT);
        let b = self.compose(std::slice::from_ref(to), output, TRANSPARENT);
        let out = self.scratch_texture(output.width, output.height);
        self.smooth_cut
            .get_or_init(|| crate::smooth_cut::SmoothCut::new(&self.device))
            .render(
                &self.device,
                &self.queue,
                &self.sampler,
                &a,
                &b,
                &out,
                progress,
            );
        give_back(&mut self.scratch.lock().unwrap(), [a, b]);
        PooledTexture {
            texture: Some(out),
            pool: Arc::clone(&self.scratch),
        }
    }

    /// `fill` composed alone and cut out by `matte`, composed alone too.
    /// Owned like `render_layers_to_owned_texture_transparent`, to be used as a
    /// `LayerContent::Texture`.
    pub fn render_matted_to_owned_texture(
        &self,
        fill: &[Layer],
        matte: &[Layer],
        mode: vv_core::MatteMode,
        output: OutputFrame,
    ) -> PooledTexture {
        use vv_core::MatteMode as M;
        let out = self.compose(fill, output, TRANSPARENT);
        let matte_texture = self.compose(matte, output, TRANSPARENT);
        let luma = matches!(mode, M::Luma | M::LumaInverted);
        let inverted = matches!(mode, M::AlphaInverted | M::LumaInverted);
        let flag = |b: bool| if b { 1.0f32 } else { 0.0 };
        let uniform: [f32; 4] = [flag(luma), flag(inverted), 0.0, 0.0];
        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vv-render matte uniform"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let matte_view = matte_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vv-render matte bind group"),
            layout: &self.work.matte.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&matte_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vv-render matte encoder"),
            });
        self.pass(
            &mut encoder,
            &out.create_view(&wgpu::TextureViewDescriptor::default()),
            wgpu::LoadOp::Load,
            Some((&bind_group, &self.work.matte)),
        );
        self.queue.submit(Some(encoder.finish()));
        give_back(&mut self.scratch.lock().unwrap(), [matte_texture]);
        PooledTexture {
            texture: Some(out),
            pool: Arc::clone(&self.scratch),
        }
    }

    fn render_resolved(
        &self,
        layers: &[Layer],
        output: OutputFrame,
        clear: wgpu::Color,
    ) -> wgpu::Texture {
        if self.work.format == OUTPUT_FORMAT {
            // Already 8 bits: nothing to resolve.
            let target = self.output_texture(output.width, output.height);
            let texture = self.compose_into(target, layers, output, clear);
            give_back(&mut self.pool.lock().unwrap().outputs, [texture.clone()]);
            return texture;
        }
        let work = self.compose(layers, output, clear);
        let resolved = self.resolve(&work);
        give_back(&mut self.scratch.lock().unwrap(), [work]);
        resolved
    }

    /// `work` to a new `OUTPUT_FORMAT` texture, dithered.
    fn resolve(&self, work: &wgpu::Texture) -> wgpu::Texture {
        let output = self.output_texture(work.width(), work.height());
        let work_view = work.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vv-render resolve bind group"),
            layout: &self.resolve_pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&work_view),
            }],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vv-render resolve encoder"),
            });
        self.pass(
            &mut encoder,
            &output.create_view(&wgpu::TextureViewDescriptor::default()),
            wgpu::LoadOp::Clear(TRANSPARENT),
            Some((&bind_group, &self.resolve_pipeline)),
        );
        self.queue.submit(Some(encoder.finish()));
        // A copy stays in the pool: the next frame of the same size draws
        // over it, after the GPU has finished with this one (same queue).
        give_back(&mut self.pool.lock().unwrap().outputs, [output.clone()]);
        output
    }

    /// The stack composed into a work texture from the scratch pool, which
    /// the caller gives back.
    fn compose(&self, layers: &[Layer], output: OutputFrame, clear: wgpu::Color) -> wgpu::Texture {
        self.compose_into(
            self.scratch_texture(output.width, output.height),
            layers,
            output,
            clear,
        )
    }

    /// The stack composed into `output_texture`, in the work format.
    fn compose_into(
        &self,
        output_texture: wgpu::Texture,
        layers: &[Layer],
        output: OutputFrame,
        clear: wgpu::Color,
    ) -> wgpu::Texture {
        let output_view = output_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut rec = self.recording();
        // No layers: only the clear remains.
        if layers.is_empty() {
            self.pass(
                &mut rec.encoder,
                &output_view,
                wgpu::LoadOp::Clear(clear),
                None,
            );
        }
        let mut first = true;
        for layer in layers {
            self.compose_layer(
                &mut rec,
                &output_texture,
                &output_view,
                layer,
                output,
                clear,
                &mut first,
            );
        }
        self.submit(rec);
        output_texture
    }

    fn recording(&self) -> Recording {
        Recording {
            encoder: self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("vv-render transform encoder"),
                }),
            planes: Vec::new(),
            scratch: Vec::new(),
        }
    }

    fn submit(&self, rec: Recording) {
        self.queue.submit(Some(rec.encoder.finish()));
        give_back(
            &mut self.scratch.lock().unwrap(),
            rec.scratch.into_iter().flatten(),
        );
        give_back(&mut self.pool.lock().unwrap().planes, rec.planes);
    }

    /// Composes `layer` onto `target`, cleared to `clear` by the first pass
    /// that reaches it (`first`, shared by the layers of the stack).
    fn compose_layer(
        &self,
        rec: &mut Recording,
        target: &wgpu::Texture,
        target_view: &wgpu::TextureView,
        layer: &Layer,
        output: OutputFrame,
        clear: wgpu::Color,
        first: &mut bool,
    ) {
        let Layer {
            content,
            transform,
            opacity,
            filters,
            blend,
            masks,
        } = layer;
        let blend = *blend;
        let is_adjustment = matches!(content, LayerContent::Adjustment);
        let no_backdrop_view = self.opaque_view.clone();
        let backdrop_view = |backdrops: &mut Vec<Option<wgpu::Texture>>| {
            let texture = (blend != BlendMode::Normal || is_adjustment)
                .then(|| self.scratch_texture(output.width, output.height));
            let view = texture.as_ref().map_or_else(
                || no_backdrop_view.clone(),
                |t| t.create_view(&wgpu::TextureViewDescriptor::default()),
            );
            backdrops.push(texture);
            view
        };
        let pipeline = if blend == BlendMode::Normal && !is_adjustment {
            &self.work.normal
        } else {
            &self.work.blend
        };
        let chain = FilterChain::new(filters, matches!(content, LayerContent::Solid(_)));
        // An adjustment has no shape of its own to cast.
        if let Some(shadow) = chain.shadow.filter(|_| !is_adjustment) {
            // Composed once by itself: the shadow is cast from it, then it
            // goes onto the stack as it is.
            let unshadowed: Vec<_> = filters
                .iter()
                .filter(|f| f.kind != vv_core::FilterKind::DropShadow)
                .copied()
                .collect();
            let alone = self.scratch_texture(output.width, output.height);
            let alone_view = alone.create_view(&wgpu::TextureViewDescriptor::default());
            self.compose_layer(
                rec,
                &alone,
                &alone_view,
                &Layer {
                    filters: &unshadowed,
                    blend: BlendMode::Normal,
                    ..*layer
                },
                output,
                TRANSPARENT,
                &mut true,
            );
            if *first {
                self.pass(
                    &mut rec.encoder,
                    target_view,
                    wgpu::LoadOp::Clear(clear),
                    None,
                );
                *first = false;
            }
            self.shadow_pass(rec, target_view, &alone, &shadow, output);
            let composed = Layer {
                blend,
                ..Layer::new(
                    LayerContent::Texture {
                        texture: &alone,
                        source_size: output.timeline_size,
                    },
                    Transform::default(),
                )
            };
            self.compose_layer(rec, target, target_view, &composed, output, clear, first);
            rec.scratch.push(Some(alone));
            return;
        }
        // Background and text are distinct passes: blended or faded one by
        // one, the background would show through the text.
        if let LayerContent::Text(title) = content
            && title.background.enabled
            && (blend != BlendMode::Normal || *opacity < 1.0)
        {
            let alone = self.scratch_texture(output.width, output.height);
            let alone_view = alone.create_view(&wgpu::TextureViewDescriptor::default());
            self.compose_layer(
                rec,
                &alone,
                &alone_view,
                &Layer {
                    blend: BlendMode::Normal,
                    opacity: 1.0,
                    ..*layer
                },
                output,
                TRANSPARENT,
                &mut true,
            );
            let composed = Layer {
                blend,
                opacity: *opacity,
                ..Layer::new(
                    LayerContent::Texture {
                        texture: &alone,
                        source_size: output.timeline_size,
                    },
                    Transform::default(),
                )
            };
            self.compose_layer(rec, target, target_view, &composed, output, clear, first);
            rec.scratch.push(Some(alone));
            return;
        }
        if let Some((_, trailing)) = chain.blurs.last() {
            if *first {
                self.pass(
                    &mut rec.encoder,
                    target_view,
                    wgpu::LoadOp::Clear(clear),
                    None,
                );
                *first = false;
            }
            let backdrop = (blend != BlendMode::Normal || is_adjustment).then(|| {
                let texture = self.scratch_texture(output.width, output.height);
                rec.encoder.copy_texture_to_texture(
                    target.as_image_copy(),
                    texture.as_image_copy(),
                    target.size(),
                );
                texture
            });
            let backdrop_view = backdrop.as_ref().map_or_else(
                || no_backdrop_view.clone(),
                |t| t.create_view(&wgpu::TextureViewDescriptor::default()),
            );
            let (blurred, fit_size, source_size) =
                self.blurred_source(rec, content, transform, output, &chain, &backdrop_view);
            let bind_group = self.texture_bind_group(
                &mut rec.planes,
                &blurred.create_view(&wgpu::TextureViewDescriptor::default()),
                fit_size,
                transform,
                output,
                source_size,
                *opacity,
                trailing,
                blend,
                &backdrop_view,
                is_adjustment.then_some(clear),
                masks,
            );
            self.pass(
                &mut rec.encoder,
                target_view,
                wgpu::LoadOp::Load,
                Some((&bind_group, pipeline)),
            );
            rec.scratch.extend([backdrop, Some(blurred)]);
            return;
        }
        let filters = &chain.leading;
        let backdrop_start = rec.scratch.len();
        let bind_groups = match content {
            LayerContent::Video { frame, source_size } => vec![self.layer_bind_group(
                &mut rec.planes,
                frame,
                transform,
                output,
                *source_size,
                (frame.width, frame.height),
                Fill::Video,
                *opacity,
                filters,
                blend,
                &backdrop_view(&mut rec.scratch),
                masks,
            )],
            LayerContent::Texture {
                texture,
                source_size,
            } => vec![self.texture_bind_group(
                &mut rec.planes,
                &texture.create_view(&wgpu::TextureViewDescriptor::default()),
                (texture.width(), texture.height()),
                transform,
                output,
                *source_size,
                *opacity,
                filters,
                blend,
                &backdrop_view(&mut rec.scratch),
                None,
                masks,
            )],
            // The copy of the stack is both the source and the backdrop.
            LayerContent::Adjustment => {
                let stack = backdrop_view(&mut rec.scratch);
                vec![self.texture_bind_group(
                    &mut rec.planes,
                    &stack,
                    (output.width, output.height),
                    transform,
                    output,
                    output.timeline_size,
                    *opacity,
                    filters,
                    blend,
                    &stack,
                    Some(clear),
                    masks,
                )]
            }
            // The color comes from the uniform: the planes are only placeholders.
            LayerContent::Solid(color) => vec![self.layer_bind_group(
                &mut rec.planes,
                &SOLID_PLACEHOLDER,
                transform,
                output,
                output.timeline_size,
                output.timeline_size,
                Fill::Solid(*color),
                *opacity,
                filters,
                blend,
                &backdrop_view(&mut rec.scratch),
                masks,
            )],
            LayerContent::Text(title) => {
                let render = crate::text::render_title(
                    title,
                    output.timeline_size,
                    (output.width, output.height),
                );
                let layers = self.title_views(&render);
                let mut groups = Vec::with_capacity(layers.len());
                for (mask_view, color) in &layers {
                    // Shadow, background and text are distinct passes: each one
                    // composes onto the earlier ones, backdrop included.
                    let view = backdrop_view(&mut rec.scratch);
                    groups.push(self.title_bind_group(
                        mask_view, *color, transform, output, *opacity, filters, blend, &view,
                        masks,
                    ));
                }
                groups
            }
        };
        for (bind_group, backdrop) in bind_groups.iter().zip(&rec.scratch[backdrop_start..]) {
            if let Some(backdrop) = backdrop {
                // The backdrop must be read from a copy: the target texture
                // is already attached to the pass composing it. If this is the
                // first layer, the clear must happen before copying it.
                if *first {
                    self.pass(
                        &mut rec.encoder,
                        target_view,
                        wgpu::LoadOp::Clear(clear),
                        None,
                    );
                    *first = false;
                }
                rec.encoder.copy_texture_to_texture(
                    target.as_image_copy(),
                    backdrop.as_image_copy(),
                    target.size(),
                );
            }
            let load = if *first {
                wgpu::LoadOp::Clear(clear)
            } else {
                wgpu::LoadOp::Load
            };
            *first = false;
            self.pass(
                &mut rec.encoder,
                target_view,
                load,
                Some((bind_group, pipeline)),
            );
        }
    }

    /// Draws the source of a layer into a texture of its own, with the
    /// identity transform, and blurs it there. The per-pixel filters of
    /// `chain` go with it, except those after the last blur, left to the pass
    /// composing it. `stack` is the source of an adjustment layer. Also returns
    /// the fit size and the crop units of that pass.
    fn blurred_source(
        &self,
        rec: &mut Recording,
        content: &LayerContent,
        transform: &Transform,
        output: OutputFrame,
        chain: &FilterChain,
        stack: &wgpu::TextureView,
    ) -> (wgpu::Texture, (u32, u32), (u32, u32)) {
        let (texture_size, source_size) = match content {
            LayerContent::Video { frame, source_size } => {
                ((frame.width, frame.height), *source_size)
            }
            LayerContent::Texture {
                texture,
                source_size,
            } => ((texture.width(), texture.height()), *source_size),
            _ => ((output.width, output.height), output.timeline_size),
        };
        let mut space = WorkSpace::of_source(texture_size, transform, output);
        let size = (space.size[0], space.size[1]);
        let no_backdrop = self.opaque_view.clone();
        let frame = OutputFrame::exact(size.0, size.1);
        let identity = Transform::default();
        let leading = &chain.leading;
        let planes = &mut rec.planes;
        let groups = match content {
            LayerContent::Video { frame: yuv, .. } => vec![self.layer_bind_group(
                planes,
                yuv,
                &identity,
                frame,
                size,
                size,
                Fill::Video,
                1.0,
                leading,
                BlendMode::Normal,
                &no_backdrop,
                &[],
            )],
            LayerContent::Texture { texture, .. } => vec![self.texture_bind_group(
                planes,
                &texture.create_view(&wgpu::TextureViewDescriptor::default()),
                size,
                &identity,
                frame,
                size,
                1.0,
                leading,
                BlendMode::Normal,
                &no_backdrop,
                None,
                &[],
            )],
            LayerContent::Adjustment => vec![self.texture_bind_group(
                planes,
                stack,
                size,
                &identity,
                frame,
                size,
                1.0,
                leading,
                BlendMode::Normal,
                &no_backdrop,
                None,
                &[],
            )],
            LayerContent::Solid(color) => vec![self.layer_bind_group(
                planes,
                &SOLID_PLACEHOLDER,
                &identity,
                frame,
                size,
                size,
                Fill::Solid(*color),
                1.0,
                leading,
                BlendMode::Normal,
                &no_backdrop,
                &[],
            )],
            LayerContent::Text(title) => {
                let render = crate::text::render_title(
                    title,
                    output.timeline_size,
                    (output.width, output.height),
                );
                self.title_views(&render)
                    .iter()
                    .map(|(mask_view, color)| {
                        self.title_bind_group(
                            mask_view,
                            *color,
                            &identity,
                            frame,
                            1.0,
                            leading,
                            BlendMode::Normal,
                            &no_backdrop,
                            &[],
                        )
                    })
                    .collect()
            }
        };
        // Alpha-over on a transparent clear: premultiplied, as `Fill::Rgba` wants.
        let mut current = self.scratch_texture(size.0, size.1);
        let current_view = current.create_view(&wgpu::TextureViewDescriptor::default());
        let mut load = wgpu::LoadOp::Clear(TRANSPARENT);
        for group in &groups {
            self.pass(
                &mut rec.encoder,
                &current_view,
                load,
                Some((group, &self.work.normal)),
            );
            load = wgpu::LoadOp::Load;
        }

        for (i, (blur, run)) in chain.blurs.iter().enumerate() {
            let blurred;
            (blurred, space) = self.blurred(rec, &current, space, *blur);
            rec.scratch
                .push(Some(std::mem::replace(&mut current, blurred)));
            if i + 1 < chain.blurs.len() && !run.is_empty() {
                let size = (space.size[0], space.size[1]);
                let target = self.scratch_texture(size.0, size.1);
                let group = self.texture_bind_group(
                    &mut rec.planes,
                    &current.create_view(&wgpu::TextureViewDescriptor::default()),
                    size,
                    &identity,
                    OutputFrame::exact(size.0, size.1),
                    size,
                    1.0,
                    run,
                    BlendMode::Normal,
                    &no_backdrop,
                    None,
                    &[],
                );
                self.pass(
                    &mut rec.encoder,
                    &target.create_view(&wgpu::TextureViewDescriptor::default()),
                    wgpu::LoadOp::Clear(TRANSPARENT),
                    Some((&group, &self.work.normal)),
                );
                rec.scratch
                    .push(Some(std::mem::replace(&mut current, target)));
            }
        }
        (current, texture_size, source_size)
    }

    /// `source`, a texture of `space`, halved along each axis until
    /// `radius` (timeline pixels per axis, 0 to leave one alone) spans at most
    /// `MAX_REACH_TEXELS`; `None` if it already does.
    fn reduced(
        &self,
        rec: &mut Recording,
        source: &wgpu::Texture,
        mut space: WorkSpace,
        radius: [f32; 2],
    ) -> (Option<wgpu::Texture>, WorkSpace) {
        let mut current: Option<wgpu::Texture> = None;
        loop {
            let halve = [0, 1].map(|axis| {
                space.size[axis] > 1 && space.texels(radius[axis], axis) > MAX_REACH_TEXELS
            });
            if halve == [false, false] {
                return (current, space);
            }
            let size = [0, 1].map(|axis| {
                if halve[axis] {
                    space.size[axis].div_ceil(2)
                } else {
                    space.size[axis]
                }
            });
            let target = self.scratch_texture(size[0], size[1]);
            self.blur_pass(
                &mut rec.encoder,
                current.as_ref().unwrap_or(source),
                &target,
                BlurUniform::RESAMPLE,
            );
            rec.scratch.push(current.replace(target));
            space = WorkSpace {
                size,
                texels_per_pixel: [0, 1].map(|axis| {
                    space.texels_per_pixel[axis] * size[axis] as f32 / space.size[axis] as f32
                }),
            };
        }
    }

    /// `source`, a texture of `space`, blurred into a new one, possibly
    /// smaller (see `reduced`): its space comes with it.
    fn blurred(
        &self,
        rec: &mut Recording,
        source: &wgpu::Texture,
        space: WorkSpace,
        blur: Blur,
    ) -> (wgpu::Texture, WorkSpace) {
        let axes = [blur.direction.horizontal(), blur.direction.vertical()];
        let (mut current, space) = self.reduced(
            rec,
            source,
            space,
            axes.map(|on| if on { blur.radius } else { 0.0 }),
        );
        for axis in (0..2).filter(|&axis| axes[axis]) {
            let target = self.scratch_texture(space.size[0], space.size[1]);
            self.blur_pass(
                &mut rec.encoder,
                current.as_ref().unwrap_or(source),
                &target,
                BlurUniform::new(
                    blur,
                    space.texels(blur.radius, axis),
                    (space.size[0], space.size[1]),
                    axis,
                ),
            );
            rec.scratch.push(current.replace(target));
        }
        (current.expect("a blur has at least one direction"), space)
    }

    /// The coverage of `source`, a texture of `space`, thickened by `radius`
    /// timeline pixels with round corners, into a new texture (alpha only),
    /// possibly smaller (see `reduced`).
    fn grown(
        &self,
        rec: &mut Recording,
        source: &wgpu::Texture,
        space: WorkSpace,
        radius: f32,
    ) -> (wgpu::Texture, WorkSpace) {
        let (reduced, space) = self.reduced(rec, source, space, [radius; 2]);
        let source = reduced.as_ref().unwrap_or(source);
        let texels = [0, 1].map(|axis| space.texels(radius, axis));
        let uniform = GrowUniform {
            params: [
                texels[0],
                texels[0].ceil() + 1.0,
                texels[1].ceil() + 1.0,
                texels[0] / texels[1],
            ],
        };
        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vv-render grow uniform"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let rows = self.scratch_texture_in(space.size[0], space.size[1], DISTANCE_FORMAT);
        let rows_view = rows.create_view(&wgpu::TextureViewDescriptor::default());
        let grown = self.scratch_texture(space.size[0], space.size[1]);
        for (pipeline, target, with_rows) in [
            (&self.work.grow_rows, &rows_view, false),
            (
                &self.work.grow,
                &grown.create_view(&Default::default()),
                true,
            ),
        ] {
            let mut entries = vec![
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ];
            if with_rows {
                entries.push(wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&rows_view),
                });
            }
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vv-render grow bind group"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &entries,
            });
            self.pass(
                &mut rec.encoder,
                target,
                wgpu::LoadOp::Clear(TRANSPARENT),
                Some((&bind_group, pipeline)),
            );
        }
        rec.scratch.extend([reduced, Some(rows)]);
        (grown, space)
    }

    /// Draws onto `target` the shadow of `layer`, the layer composed by
    /// itself onto a transparent texture as large as the output.
    fn shadow_pass(
        &self,
        rec: &mut Recording,
        target: &wgpu::TextureView,
        layer: &wgpu::Texture,
        shadow: &vv_core::FilterValue,
        output: OutputFrame,
    ) {
        let blur = Blur {
            gaussian: true,
            radius: shadow.radius,
            direction: vv_core::BlurDirection::Both,
        };
        let mut space = WorkSpace::of_output(output);
        let mut cast = None;
        if shadow.grow > 0.0 {
            let grown;
            (grown, space) = self.grown(rec, layer, space, shadow.grow);
            cast = Some(grown);
        }
        if blur.radius > 0.0 {
            let (blurred, _) = self.blurred(rec, cast.as_ref().unwrap_or(layer), space, blur);
            rec.scratch.push(cast.replace(blurred));
        }
        let (timeline_w, timeline_h) = (
            output.timeline_size.0.max(1) as f32,
            output.timeline_size.1.max(1) as f32,
        );
        let uniform = ShadowUniform {
            color: [
                shadow.color.r,
                shadow.color.g,
                shadow.color.b,
                shadow.color.a * (shadow.amount / 100.0).clamp(0.0, 1.0),
            ],
            // Y upwards in the timeline, downwards in uv.
            offset: [
                shadow.offset[0] / timeline_w,
                -shadow.offset[1] / timeline_h,
                0.0,
                0.0,
            ],
        };
        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vv-render shadow uniform"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let cast_view = cast
            .as_ref()
            .unwrap_or(layer)
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vv-render shadow bind group"),
            layout: &self.work.shadow.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&cast_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        self.pass(
            &mut rec.encoder,
            target,
            wgpu::LoadOp::Load,
            Some((&bind_group, &self.work.shadow)),
        );
        rec.scratch.push(cast);
    }

    fn blur_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::Texture,
        target: &wgpu::Texture,
        uniform: BlurUniform,
    ) {
        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vv-render blur uniform"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vv-render blur bind group"),
            layout: &self.work.blur.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        self.pass(
            encoder,
            &target.create_view(&wgpu::TextureViewDescriptor::default()),
            wgpu::LoadOp::Clear(TRANSPARENT),
            Some((&bind_group, &self.work.blur)),
        );
    }

    /// Maps `buffer` for reading (waiting for the GPU) and passes the bytes to `read`.
    fn map_read<R>(&self, buffer: &wgpu::Buffer, read: impl FnOnce(&[u8]) -> R) -> R {
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("wgpu device poll failed");
        rx.recv()
            .expect("the map_async callback did not answer")
            .expect("map_async failed");
        let out = read(&slice.get_mapped_range().expect("get_mapped_range failed"));
        buffer.unmap();
        out
    }

    /// Uploads the three planes of the layer and the bind group ready for the pass:
    /// crop/zoom, letterbox and YUV→RGB conversion are all in the
    /// shader, here only its inputs are prepared.
    /// Uploaded views of `render`'s masks, cached by `TitleRender::id`.
    fn title_views(
        &self,
        render: &crate::text::TitleRender,
    ) -> Vec<(wgpu::TextureView, vv_core::Rgba)> {
        const TITLE_POOL_CAP: usize = 16;
        let mut pool = self.pool.lock().unwrap();
        if let Some(pos) = pool.titles.iter().position(|(id, _)| *id == render.id) {
            let entry = pool.titles.remove(pos);
            let out = entry
                .1
                .iter()
                .map(|(_, view, color)| (view.clone(), *color))
                .collect();
            pool.titles.push(entry);
            return out;
        }
        drop(pool);
        let mut uploaded = Vec::with_capacity(render.layers.len());
        let mut out = Vec::with_capacity(render.layers.len());
        for (mask, color) in &render.layers {
            let texture = self.device.create_texture_with_data(
                &self.queue,
                &wgpu::TextureDescriptor {
                    label: Some("vv-render title mask"),
                    size: wgpu::Extent3d {
                        width: mask.width,
                        height: mask.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: PLANE_FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &mask.data,
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            out.push((view.clone(), *color));
            uploaded.push((texture, view, *color));
        }
        let mut pool = self.pool.lock().unwrap();
        if pool.titles.len() >= TITLE_POOL_CAP {
            pool.titles.remove(0);
        }
        pool.titles.push((render.id, uploaded));
        out
    }

    fn title_bind_group(
        &self,
        mask_view: &wgpu::TextureView,
        color: vv_core::Rgba,
        transform: &Transform,
        output: OutputFrame,
        opacity: f32,
        filters: &[vv_core::FilterValue],
        blend: BlendMode,
        backdrop: &wgpu::TextureView,
        masks: &[vv_core::MaskValue],
    ) -> wgpu::BindGroup {
        let size = output.timeline_size;
        let uniform = TransformUniform::new(
            transform,
            ColorMatrix::Bt601,
            true,
            fit_factors(
                (size.0.max(1) as f32, size.1.max(1) as f32),
                (output.width as f32, output.height as f32),
            ),
            output,
            size,
            Fill::Mask(color),
            opacity,
            filters,
            blend,
        );
        let mask_uniform = (!masks.is_empty()).then(|| MaskUniform::new(masks, transform, output));
        let grade_uniform = filters
            .iter()
            .any(|f| f.kind == vv_core::FilterKind::ColorCorrection)
            .then(|| GradeUniform::new(filters));
        self.bind_group_for(
            [
                mask_view,
                &self.neutral_view,
                &self.neutral_view,
                &self.opaque_view,
                backdrop,
            ],
            &uniform,
            mask_uniform.as_ref(),
            grade_uniform.as_ref(),
        )
    }

    fn layer_bind_group(
        &self,
        planes: &mut Vec<wgpu::Texture>,
        frame: &YuvFrame,
        transform: &Transform,
        output: OutputFrame,
        source_size: (u32, u32),
        fit_size: (u32, u32),
        fill: Fill,
        opacity: f32,
        filters: &[vv_core::FilterValue],
        blend: BlendMode,
        backdrop: &wgpu::TextureView,
        masks: &[vv_core::MaskValue],
    ) -> wgpu::BindGroup {
        let y_owned = !(frame.width == 1 && frame.height == 1 && frame.y == [0]);
        let y_view_owned = y_owned.then(|| {
            let t = self.plane_texture(frame.y, frame.width, frame.height);
            let v = t.create_view(&wgpu::TextureViewDescriptor::default());
            planes.push(t);
            v
        });
        let y_view = y_view_owned.as_ref().unwrap_or(&self.zero_view);

        let mut u_view_owned = None;
        let mut v_view_owned = None;
        let (u_view, v_view) = match frame.chroma {
            YuvChroma::Planar { u, v }
                if frame.chroma_width == 1
                    && frame.chroma_height == 1
                    && u == [128]
                    && v == [128] =>
            {
                (&self.neutral_view, &self.neutral_view)
            }
            YuvChroma::Planar { u, v } => {
                let ut = self.plane_texture(u, frame.chroma_width, frame.chroma_height);
                let vt = self.plane_texture(v, frame.chroma_width, frame.chroma_height);
                let uv =
                    u_view_owned.insert(ut.create_view(&wgpu::TextureViewDescriptor::default()));
                let vv =
                    v_view_owned.insert(vt.create_view(&wgpu::TextureViewDescriptor::default()));
                planes.extend([ut, vt]);
                (&*uv, &*vv)
            }
            YuvChroma::Interleaved(uv) => {
                let ut =
                    self.texture_with(uv, frame.chroma_width, frame.chroma_height, UV_PLANE_FORMAT);
                let uv_view =
                    u_view_owned.insert(ut.create_view(&wgpu::TextureViewDescriptor::default()));
                planes.push(ut);
                (&*uv_view, &self.neutral_view)
            }
        };

        // A single `[255]` byte = "opaque everywhere" placeholder: bind the
        // permanent 1x1 opaque texture view without touching the pool.
        let a_view_owned = (frame.alpha != OPAQUE).then(|| {
            let (alpha_w, alpha_h) = if frame.alpha.len() == 1 {
                (1, 1)
            } else {
                (frame.width, frame.height)
            };
            let t = self.plane_texture(frame.alpha, alpha_w, alpha_h);
            let v = t.create_view(&wgpu::TextureViewDescriptor::default());
            planes.push(t);
            v
        });
        let a_view = a_view_owned.as_ref().unwrap_or(&self.opaque_view);

        let mut uniform = TransformUniform::new(
            transform,
            frame.matrix,
            frame.full_range,
            fit_factors(
                (fit_size.0.max(1) as f32, fit_size.1.max(1) as f32),
                (output.width as f32, output.height as f32),
            ),
            output,
            source_size,
            fill,
            opacity,
            filters,
            blend,
        );
        if matches!(frame.chroma, YuvChroma::Interleaved(_)) {
            uniform.planes[0] = 1.0;
        }
        let mask_uniform = (!masks.is_empty()).then(|| MaskUniform::new(masks, transform, output));
        let grade_uniform = filters
            .iter()
            .any(|f| f.kind == vv_core::FilterKind::ColorCorrection)
            .then(|| GradeUniform::new(filters));
        self.bind_group_for(
            [y_view, u_view, v_view, a_view, backdrop],
            &uniform,
            mask_uniform.as_ref(),
            grade_uniform.as_ref(),
        )
    }

    /// Like `layer_bind_group`, but the source is an already composed RGBA
    /// texture (`LayerContent::Texture`), letterboxed as if it were `fit_size`: it takes the Y plane slot — the layout
    /// only asks for a filterable float 2D texture, and the work format satisfies
    /// it as much as `R8Unorm` — and the other slots take the 1x1
    /// placeholders, which with `Fill::Rgba` the shader does not sample (except the alpha,
    /// which must stay opaque). `adjustment_clear` = draw it as an adjustment
    /// layer (`LayerContent::Adjustment`) over a timeline with that clear.
    fn texture_bind_group(
        &self,
        _planes: &mut Vec<wgpu::Texture>,
        rgba_view: &wgpu::TextureView,
        fit_size: (u32, u32),
        transform: &Transform,
        output: OutputFrame,
        source_size: (u32, u32),
        opacity: f32,
        filters: &[vv_core::FilterValue],
        blend: BlendMode,
        backdrop: &wgpu::TextureView,
        adjustment_clear: Option<wgpu::Color>,
        masks: &[vv_core::MaskValue],
    ) -> wgpu::BindGroup {
        let mut uniform = TransformUniform::new(
            transform,
            ColorMatrix::Bt709,
            false,
            fit_factors(
                (fit_size.0.max(1) as f32, fit_size.1.max(1) as f32),
                (output.width as f32, output.height as f32),
            ),
            output,
            source_size,
            Fill::Rgba,
            opacity,
            filters,
            blend,
        );
        if let Some(clear) = adjustment_clear {
            // The opacity moves from the layer's coverage to the final mix.
            uniform.extra = [1.0, uniform.extra[1], opacity.clamp(0.0, 1.0), 1.0];
            uniform.solid = [
                clear.r as f32,
                clear.g as f32,
                clear.b as f32,
                clear.a as f32,
            ];
        }
        let mask_uniform = (!masks.is_empty()).then(|| MaskUniform::new(masks, transform, output));
        let grade_uniform = filters
            .iter()
            .any(|f| f.kind == vv_core::FilterKind::ColorCorrection)
            .then(|| GradeUniform::new(filters));
        self.bind_group_for(
            [
                rgba_view,
                &self.neutral_view,
                &self.neutral_view,
                &self.opaque_view,
                backdrop,
            ],
            &uniform,
            mask_uniform.as_ref(),
            grade_uniform.as_ref(),
        )
    }

    /// The bind group of the pass: the views in the order `[source, U, V, alpha,
    /// backdrop]` (the first is the Y plane or the RGBA texture, see `Fill`).
    fn bind_group_for(
        &self,
        views: [&wgpu::TextureView; 5],
        uniform: &TransformUniform,
        masks: Option<&MaskUniform>,
        grade: Option<&GradeUniform>,
    ) -> wgpu::BindGroup {
        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vv-render transform uniform"),
                contents: bytemuck::bytes_of(uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let mask_buffer_owned = masks.map(|masks| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("vv-render mask uniform"),
                    contents: bytemuck::bytes_of(masks),
                    usage: wgpu::BufferUsages::UNIFORM,
                })
        });
        let mask_buffer = mask_buffer_owned
            .as_ref()
            .unwrap_or(&self.empty_mask_buffer);
        let grade_buffer_owned = grade.map(|grade| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("vv-render grade uniform"),
                    contents: bytemuck::bytes_of(grade),
                    usage: wgpu::BufferUsages::UNIFORM,
                })
        });
        let grade_buffer = grade_buffer_owned
            .as_ref()
            .unwrap_or(&self.neutral_grade_buffer);
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vv-render transform bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(views[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(views[3]),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(views[4]),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: mask_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: grade_buffer.as_entire_binding(),
                },
            ],
        })
    }

    /// An R8 plane with `data`, taken from the pool if there is one of the same
    /// size.
    fn plane_texture(&self, data: &[u8], width: u32, height: u32) -> wgpu::Texture {
        self.texture_with(data, width, height, PLANE_FORMAT)
    }

    fn texture_with(
        &self,
        data: &[u8],
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> wgpu::Texture {
        let pooled = take_sized(&mut self.pool.lock().unwrap().planes, width, height, format);
        let texture = pooled.unwrap_or_else(|| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("vv-render plane"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        });
        self.queue.write_texture(
            texture.as_image_copy(),
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * format.block_copy_size(None).unwrap()),
                rows_per_image: Some(height),
            },
            texture.size(),
        );
        texture
    }

    /// A work texture, from the scratch pool (see `PooledTexture`): separate
    /// from `output_texture`'s because there a texture stays taken until
    /// whoever uses it gives it back.
    fn scratch_texture(&self, output_w: u32, output_h: u32) -> wgpu::Texture {
        self.scratch_texture_in(output_w, output_h, self.work.format)
    }

    fn scratch_texture_in(
        &self,
        output_w: u32,
        output_h: u32,
        format: wgpu::TextureFormat,
    ) -> wgpu::Texture {
        take_sized(
            &mut self.scratch.lock().unwrap(),
            output_w,
            output_h,
            format,
        )
        .unwrap_or_else(|| self.new_output_texture(output_w, output_h, format))
    }

    fn output_texture(&self, output_w: u32, output_h: u32) -> wgpu::Texture {
        if let Some(texture) = take_sized(
            &mut self.pool.lock().unwrap().outputs,
            output_w,
            output_h,
            OUTPUT_FORMAT,
        ) {
            return texture;
        }
        self.new_output_texture(output_w, output_h, OUTPUT_FORMAT)
    }

    fn new_output_texture(
        &self,
        output_w: u32,
        output_h: u32,
        format: wgpu::TextureFormat,
    ) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vv-render output frame"),
            size: wgpu::Extent3d {
                width: output_w,
                height: output_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            // TEXTURE_BINDING is needed by the zero-copy path: egui-wgpu samples it.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    }

    /// One pass on the output texture: `bind_group` absent = only the
    /// `load` (clear of a solid color or of black), no draw.
    fn pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        output_view: &wgpu::TextureView,
        load: wgpu::LoadOp<wgpu::Color>,
        bind_group: Option<(&wgpu::BindGroup, &wgpu::RenderPipeline)>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vv-render transform pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: output_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some((bind_group, pipeline)) = bind_group {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

/// Letterbox/pillarbox factors passed to the shader: >1 on the axis that
/// stays uncovered (black bars), 1 on the other.
fn fit_factors(source: (f32, f32), output: (f32, f32)) -> [f32; 2] {
    let source_aspect = source.0 / source.1;
    let output_aspect = output.0 / output.1;
    if source_aspect > output_aspect {
        [1.0, source_aspect / output_aspect]
    } else {
        [output_aspect / source_aspect, 1.0]
    }
}

/// Dimensions with the aspect ratio of `aspect` containing `source` without
/// scaling it: only the bars are added.
pub fn fit_output_size(source: (u32, u32), aspect: (u32, u32)) -> (u32, u32) {
    let (sw, sh) = (source.0.max(1) as f64, source.1.max(1) as f64);
    let (aw, ah) = (aspect.0.max(1) as f64, aspect.1.max(1) as f64);
    if sw / sh > aw / ah {
        (source.0.max(1), ((sw * ah / aw).round() as u32).max(1))
    } else {
        (((sh * aw / ah).round() as u32).max(1), source.1.max(1))
    }
}

#[cfg(test)]
impl<'a> YuvFrame<'a> {
    fn borrowed(&self) -> YuvFrame<'a> {
        YuvFrame { ..*self }
    }
}

impl Compositor {
    /// Composes the stack in alpha-over on opaque black and reads the result
    /// in RGBA8.
    pub fn render_layers(&self, layers: &[Layer], output: OutputFrame) -> Vec<u8> {
        let output_texture = self.render_layers_to_texture(layers, output);
        self.read_rgba_texture(&output_texture, output.width, output.height)
    }
}

#[cfg(test)]
impl Compositor {
    /// A single frame with `transform`, read in RGBA8.
    pub fn render_frame(
        &self,
        frame: &YuvFrame,
        transform: &Transform,
        output: OutputFrame,
    ) -> Vec<u8> {
        self.render_layers(
            &[Layer::new(
                LayerContent::Video {
                    frame: frame.borrowed(),
                    source_size: (frame.width, frame.height),
                },
                *transform,
            )],
            output,
        )
    }
    /// Like `render_frame` but stays on the GPU. No waiting: the egui pass
    /// sampling it is submitted afterwards on the same queue.
    pub fn render_frame_to_texture(
        &self,
        frame: &YuvFrame,
        transform: &Transform,
        output: OutputFrame,
    ) -> wgpu::Texture {
        self.render_layers_to_texture(
            &[Layer::new(
                LayerContent::Video {
                    frame: frame.borrowed(),
                    source_size: (frame.width, frame.height),
                },
                *transform,
            )],
            output,
        )
    }
}

#[cfg(test)]
#[path = "tests/compositor.rs"]
mod tests;
