use serde::Serialize;
use wgpu_profiler::{GpuProfiler, GpuProfilerSettings};

pub const FEATURES: wgpu::Features =
    wgpu::Features::TIMESTAMP_QUERY.union(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);

#[derive(Serialize)]
pub struct GpuSample {
    pub label: String,
    pub elapsed_ms: f64,
}

#[derive(Default, Serialize)]
pub struct GpuReport {
    pub submissions: usize,
    pub samples: Vec<GpuSample>,
}

pub(crate) struct GpuTelemetry {
    profiler: GpuProfiler,
    report: GpuReport,
}

impl GpuTelemetry {
    pub fn new(device: &wgpu::Device) -> Option<Self> {
        if !device.features().contains(FEATURES) {
            return None;
        }
        Some(Self {
            profiler: GpuProfiler::new(
                device,
                GpuProfilerSettings {
                    // The timestamp pair spans two command buffers.
                    enable_debug_groups: false,
                    max_num_pending_frames: 64,
                    ..Default::default()
                },
            )
            .ok()?,
            report: GpuReport::default(),
        })
    }

    pub fn submit(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mut encoder: wgpu::CommandEncoder,
        label: &str,
    ) {
        self.collect(queue);
        let mut start = device.create_command_encoder(&Default::default());
        let query = self.profiler.begin_query(label, &mut start);
        self.profiler.end_query(&mut encoder, query);
        self.profiler.resolve_queries(&mut encoder);
        queue.submit([start.finish(), encoder.finish()]);
        self.profiler.end_frame().expect("balanced GPU queries");
        self.report.submissions += 1;
    }

    fn collect(&mut self, queue: &wgpu::Queue) {
        while let Some(results) = self
            .profiler
            .process_finished_frame(queue.get_timestamp_period())
        {
            for result in results {
                if let Some(time) = result.time {
                    self.report.samples.push(GpuSample {
                        label: result.label,
                        elapsed_ms: (time.end - time.start) * 1000.0,
                    });
                }
            }
        }
    }

    pub fn finish(&mut self, queue: &wgpu::Queue) -> GpuReport {
        self.collect(queue);
        std::mem::take(&mut self.report)
    }
}

#[cfg(test)]
#[path = "tests/telemetry.rs"]
mod tests;
