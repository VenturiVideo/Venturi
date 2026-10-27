//! Video scopes of a composed frame, computed on the GPU: the pixels are
//! counted into bins, then the bins are drawn into a texture for egui. The
//! graticule is left to whoever shows it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ScopeKind {
    Waveform,
    Parade,
    Vectorscope,
    Histogram,
}

impl ScopeKind {
    pub const ALL: [Self; 4] = [
        Self::Waveform,
        Self::Parade,
        Self::Vectorscope,
        Self::Histogram,
    ];

    fn mode(self) -> u32 {
        self as u32
    }
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    mode: u32,
    bins_w: u32,
    src_w: u32,
    src_h: u32,
    step: u32,
    out_w: u32,
    out_h: u32,
    _pad: u32,
    scale: f32,
    _pad1: [f32; 3],
}

/// Columns of a waveform (of each channel of the parade) at most.
const MAX_BINS_W: u32 = 512;
/// The source is sampled down to about this many pixels per axis: plenty
/// for a scope, and it bounds the atomic additions.
const MAX_SAMPLES: u32 = 1024;
/// The parade's, the largest bin layout.
const BIN_COUNT: u64 = 3 * 256 * MAX_BINS_W as u64;

pub struct Scopes {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    accumulate: wgpu::ComputePipeline,
    histogram_max: wgpu::ComputePipeline,
    draw: wgpu::ComputePipeline,
    bins: wgpu::Buffer,
    params: wgpu::Buffer,
    /// The picture of each slot, redrawn in place while its size holds.
    outputs: Mutex<HashMap<usize, wgpu::Texture>>,
}

impl Scopes {
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vv-render scopes shader"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("shaders/color.wgsl"),
                    include_str!("shaders/scopes.wgsl")
                )
                .into(),
            ),
        });
        let pipeline = |entry_point| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry_point),
                layout: None,
                module: &module,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let (accumulate, histogram_max, draw) = (
            pipeline("accumulate"),
            pipeline("histogram_max"),
            pipeline("draw"),
        );
        let bins = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vv-render scope bins"),
            size: BIN_COUNT * 4,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vv-render scope params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            device,
            queue,
            accumulate,
            histogram_max,
            draw,
            bins,
            params,
            outputs: Mutex::default(),
        }
    }

    /// The `kind` scope of `source` (an RGBA texture, e.g. the viewer's), as an
    /// `Rgba8Unorm` texture of `size` for the display slot `slot`. Black
    /// background, no graticule.
    pub fn render(
        &self,
        source: &wgpu::Texture,
        kind: ScopeKind,
        slot: usize,
        size: (u32, u32),
    ) -> wgpu::Texture {
        let (out_w, out_h) = (size.0.max(1), size.1.max(1));
        let output = self.output(slot, out_w, out_h);
        let (src_w, src_h) = (source.width().max(1), source.height().max(1));
        let step = src_w.div_ceil(MAX_SAMPLES).max(src_h.div_ceil(MAX_SAMPLES));
        let (samples_w, samples_h) = (src_w.div_ceil(step), src_h.div_ceil(step));
        let columns = match kind {
            ScopeKind::Parade => out_w / 3,
            _ => out_w,
        };
        let bins_w = columns.clamp(16, MAX_BINS_W).min(samples_w);
        let samples = (samples_w * samples_h) as f32;
        // A smooth gradient spreads a column over all 256 levels: such a
        // trace comes out about 60 % bright, a flat area saturates.
        let scale = match kind {
            ScopeKind::Waveform | ScopeKind::Parade => 256.0 * bins_w as f32 / samples,
            ScopeKind::Vectorscope => 2048.0 / samples,
            ScopeKind::Histogram => 0.0,
        };
        let params = Params {
            mode: kind.mode(),
            bins_w,
            src_w,
            src_h,
            step,
            out_w,
            out_h,
            _pad: 0,
            scale,
            _pad1: [0.0; 3],
        };
        self.queue
            .write_buffer(&self.params, 0, bytemuck::bytes_of(&params));

        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = |pipeline: &wgpu::ComputePipeline,
                          entries: &[(u32, wgpu::BindingResource)]| {
            let entries: Vec<_> = entries
                .iter()
                .map(|(binding, resource)| wgpu::BindGroupEntry {
                    binding: *binding,
                    resource: resource.clone(),
                })
                .collect();
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vv-render scopes bind group"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &entries,
            })
        };
        let accumulate = bind_group(
            &self.accumulate,
            &[
                (0, wgpu::BindingResource::TextureView(&source_view)),
                (1, self.bins.as_entire_binding()),
                (2, self.params.as_entire_binding()),
            ],
        );
        let histogram_max = bind_group(&self.histogram_max, &[(1, self.bins.as_entire_binding())]);
        let draw = bind_group(
            &self.draw,
            &[
                (1, self.bins.as_entire_binding()),
                (2, self.params.as_entire_binding()),
                (3, wgpu::BindingResource::TextureView(&output_view)),
            ],
        );

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vv-render scopes encoder"),
            });
        encoder.clear_buffer(&self.bins, 0, None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vv-render scopes pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.accumulate);
            pass.set_bind_group(0, &accumulate, &[]);
            pass.dispatch_workgroups(samples_w.div_ceil(16), samples_h.div_ceil(16), 1);
            if kind == ScopeKind::Histogram {
                pass.set_pipeline(&self.histogram_max);
                pass.set_bind_group(0, &histogram_max, &[]);
                pass.dispatch_workgroups(1, 1, 1);
            }
            pass.set_pipeline(&self.draw);
            pass.set_bind_group(0, &draw, &[]);
            pass.dispatch_workgroups(out_w.div_ceil(8), out_h.div_ceil(8), 1);
        }
        self.queue.submit(Some(encoder.finish()));
        output
    }

    fn output(&self, slot: usize, width: u32, height: u32) -> wgpu::Texture {
        let mut outputs = self.outputs.lock().unwrap();
        if let Some(texture) = outputs
            .get(&slot)
            .filter(|t| t.width() == width && t.height() == height)
        {
            return texture.clone();
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vv-render scope"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        outputs.insert(slot, texture.clone());
        texture
    }
}

#[cfg(test)]
#[path = "tests/scopes.rs"]
mod tests;
