//! Smooth Cut: optical flow between the two sides of a cut (pyramidal
//! Lucas-Kanade, `shaders/flow.wgsl`) and the morph between them
//! (`shaders/morph.wgsl`).

use std::sync::Mutex;

use wgpu::util::DeviceExt;

/// The flow is estimated at most this wide (long side): plenty for the
/// motion of a jump cut, and its cost stays the same at any output size.
const FLOW_MAX_SIDE: u32 = 640;
/// The coarsest level is no smaller than this (short side).
const MIN_LEVEL_SIDE: u32 = 8;
const MAX_LEVELS: usize = 7;
const LK_ITERATIONS: usize = 4;
/// The coarse levels carry the large motion and cost next to nothing.
const COARSE_LK_ITERATIONS: usize = 12;
const LK_RADIUS: i32 = 4;
const LUMA: [f32; 4] = [0.2126, 0.7152, 0.0722, 0.0];
const FLOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
const WORKGROUP: u32 = 8;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    weights: [f32; 4],
    dst_size: [u32; 2],
    use_flow: u32,
    radius: i32,
    /// `lk`: damping; `smooth`: luminance tolerance.
    lambda: f32,
    max_step: f32,
    sigma: f32,
    _pad: f32,
}

impl Params {
    fn new(dst_size: (u32, u32)) -> Self {
        Self {
            weights: [0.0; 4],
            dst_size: [dst_size.0, dst_size.1],
            use_flow: 0,
            radius: 0,
            lambda: 0.0,
            max_step: 0.0,
            sigma: 1.0,
            _pad: 0.0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MorphParams {
    out_size: [f32; 2],
    flow_scale: [f32; 2],
    progress: f32,
    _pad: [f32; 3],
}

pub(crate) struct SmoothCut {
    compute_layout: wgpu::BindGroupLayout,
    reduce: wgpu::ComputePipeline,
    upsample: wgpu::ComputePipeline,
    lk: wgpu::ComputePipeline,
    smooth: wgpu::ComputePipeline,
    morph_layout: wgpu::BindGroupLayout,
    morph_module: wgpu::ShaderModule,
    /// One per work format: it can change with the precision.
    morph_pipelines: Mutex<Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>>,
    dummy: wgpu::Texture,
    pool: Mutex<Vec<wgpu::Texture>>,
}

fn texture_entry(
    binding: u32,
    filterable: bool,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Sizes of the pyramid, finest first.
fn level_sizes(output: (u32, u32)) -> Vec<(u32, u32)> {
    let long = output.0.max(output.1).max(1);
    let scale = (FLOW_MAX_SIDE as f32 / long as f32).min(1.0);
    let mut size = (
        ((output.0 as f32 * scale).round() as u32).max(1),
        ((output.1 as f32 * scale).round() as u32).max(1),
    );
    let mut sizes = vec![size];
    while sizes.len() < MAX_LEVELS {
        let next = (size.0.div_ceil(2), size.1.div_ceil(2));
        if next.0.min(next.1) < MIN_LEVEL_SIDE {
            break;
        }
        sizes.push(next);
        size = next;
    }
    sizes
}

impl SmoothCut {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let compute = wgpu::ShaderStages::COMPUTE;
        let compute_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vv-render flow bind group layout"),
            entries: &[
                texture_entry(0, false, compute),
                texture_entry(1, false, compute),
                texture_entry(2, false, compute),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: compute,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: FLOW_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                uniform_entry(4, compute),
            ],
        });
        let compute_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vv-render flow pipeline layout"),
                bind_group_layouts: &[Some(&compute_layout)],
                immediate_size: 0,
            });
        let flow_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vv-render flow shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/flow.wgsl").into()),
        });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&compute_pipeline_layout),
                module: &flow_module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };

        let fragment = wgpu::ShaderStages::FRAGMENT;
        let morph_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vv-render morph bind group layout"),
            entries: &[
                texture_entry(0, true, fragment),
                texture_entry(1, true, fragment),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: fragment,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture_entry(3, false, fragment),
                texture_entry(4, false, fragment),
                uniform_entry(5, fragment),
            ],
        });
        let morph_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vv-render morph shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/morph.wgsl").into()),
        });
        let dummy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vv-render flow dummy"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FLOW_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        Self {
            reduce: pipeline("reduce"),
            upsample: pipeline("upsample"),
            lk: pipeline("lk"),
            smooth: pipeline("smooth_flow"),
            compute_layout,
            morph_layout,
            morph_module,
            morph_pipelines: Mutex::default(),
            dummy,
            pool: Mutex::default(),
        }
    }

    fn morph_pipeline(
        &self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        let mut pipelines = self.morph_pipelines.lock().unwrap();
        if let Some((_, pipeline)) = pipelines.iter().find(|(f, _)| *f == format) {
            return pipeline.clone();
        }
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vv-render morph pipeline layout"),
            bind_group_layouts: &[Some(&self.morph_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vv-render morph pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &self.morph_module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.morph_module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
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
        pipelines.push((format, pipeline.clone()));
        pipeline
    }

    fn flow_texture(&self, device: &wgpu::Device, size: (u32, u32)) -> wgpu::Texture {
        let mut pool = self.pool.lock().unwrap();
        if let Some(i) = pool
            .iter()
            .position(|t| t.width() == size.0 && t.height() == size.1)
        {
            return pool.swap_remove(i);
        }
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vv-render flow level"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FLOW_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        })
    }

    /// Writes into `out` (a render target of the size of `a`/`b`, premultiplied
    /// RGBA) the frame at `progress` (0..1) of the morph from `a` to `b`.
    pub(crate) fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sampler: &wgpu::Sampler,
        a: &wgpu::Texture,
        b: &wgpu::Texture,
        out: &wgpu::Texture,
        progress: f32,
    ) {
        let output = (out.width(), out.height());
        let sizes = level_sizes(output);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vv-render smooth cut encoder"),
        });
        let mut used = Vec::new();
        let take = |size: (u32, u32), used: &mut Vec<wgpu::Texture>| {
            let texture = self.flow_texture(device, size);
            used.push(texture.clone());
            texture
        };

        let pyramid = |image: &wgpu::Texture,
                       encoder: &mut wgpu::CommandEncoder,
                       used: &mut Vec<wgpu::Texture>| {
            let mut levels: Vec<wgpu::Texture> = Vec::with_capacity(sizes.len());
            for (i, &size) in sizes.iter().enumerate() {
                let level = take(size, used);
                let mut params = Params::new(size);
                params.weights = if i == 0 { LUMA } else { [1.0, 0.0, 0.0, 0.0] };
                let source = if i == 0 { image } else { &levels[i - 1] };
                self.dispatch(
                    device,
                    encoder,
                    &self.reduce,
                    [source, &self.dummy, &self.dummy],
                    &level,
                    params,
                );
                levels.push(level);
            }
            levels
        };
        let pyramid_a = pyramid(a, &mut encoder, &mut used);
        let pyramid_b = pyramid(b, &mut encoder, &mut used);

        let flow = |from: &[wgpu::Texture],
                    to: &[wgpu::Texture],
                    encoder: &mut wgpu::CommandEncoder,
                    used: &mut Vec<wgpu::Texture>| {
            let mut current: Option<wgpu::Texture> = None;
            for level in (0..sizes.len()).rev() {
                let size = sizes[level];
                if let Some(coarse) = current.take() {
                    let up = take(size, used);
                    self.dispatch(
                        device,
                        encoder,
                        &self.upsample,
                        [&coarse, &self.dummy, &self.dummy],
                        &up,
                        Params::new(size),
                    );
                    current = Some(up);
                }
                let iterations = if level + 2 >= sizes.len() {
                    COARSE_LK_ITERATIONS
                } else {
                    LK_ITERATIONS
                };
                for _ in 0..iterations {
                    let next = take(size, used);
                    let mut params = Params::new(size);
                    params.use_flow = u32::from(current.is_some());
                    params.radius = LK_RADIUS;
                    params.sigma = LK_RADIUS as f32 * 0.6;
                    params.lambda = 1e-3;
                    params.max_step = 2.0;
                    let flow_in = current.as_ref().unwrap_or(&self.dummy);
                    self.dispatch(
                        device,
                        encoder,
                        &self.lk,
                        [&from[level], &to[level], flow_in],
                        &next,
                        params,
                    );
                    current = Some(next);
                }
                let smoothed = take(size, used);
                let mut params = Params::new(size);
                params.radius = 2;
                params.sigma = 1.5;
                params.lambda = 0.08;
                self.dispatch(
                    device,
                    encoder,
                    &self.smooth,
                    [current.as_ref().unwrap(), &from[level], &self.dummy],
                    &smoothed,
                    params,
                );
                current = Some(smoothed);
            }
            current.expect("at least one level")
        };
        let flow_ab = flow(&pyramid_a, &pyramid_b, &mut encoder, &mut used);
        let flow_ba = flow(&pyramid_b, &pyramid_a, &mut encoder, &mut used);

        let params = MorphParams {
            out_size: [output.0 as f32, output.1 as f32],
            flow_scale: [
                output.0 as f32 / sizes[0].0 as f32,
                output.1 as f32 / sizes[0].1 as f32,
            ],
            progress: progress.clamp(0.0, 1.0),
            _pad: [0.0; 3],
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vv-render morph params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let (a_view, b_view, ab_view, ba_view) = (view(a), view(b), view(&flow_ab), view(&flow_ba));
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vv-render morph bind group"),
            layout: &self.morph_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&a_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&b_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&ab_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&ba_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let pipeline = self.morph_pipeline(device, out.format());
        {
            let out_view = view(out);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vv-render morph pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &out_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
        // Safe to reuse right away: later work goes on the same queue.
        let mut pool = self.pool.lock().unwrap();
        pool.extend(used);
        let excess = pool.len().saturating_sub(64);
        pool.drain(..excess);
    }

    fn dispatch(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::ComputePipeline,
        sources: [&wgpu::Texture; 3],
        dst: &wgpu::Texture,
        params: Params,
    ) {
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vv-render flow params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let views = sources.map(view);
        let dst_view = view(dst);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vv-render flow bind group"),
            layout: &self.compute_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&views[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&dst_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("vv-render flow pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            params.dst_size[0].div_ceil(WORKGROUP),
            params.dst_size[1].div_ceil(WORKGROUP),
            1,
        );
    }
}

#[cfg(test)]
#[path = "tests/smooth_cut.rs"]
mod tests;
