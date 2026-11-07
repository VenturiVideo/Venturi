use super::*;

#[test]
fn gpu_timing_requires_features_and_collects_submissions() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = match pollster::block_on(instance.request_adapter(&Default::default())) {
        Ok(adapter) => adapter,
        Err(error) => {
            eprintln!("Skipping timestamp collection: no Vulkan adapter ({error})");
            return;
        }
    };
    let (device, _) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
    assert!(GpuTelemetry::new(&device).is_none());
    if !adapter.features().contains(FEATURES) {
        eprintln!("Skipping timestamp collection: adapter lacks timestamp features");
        return;
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: FEATURES,
        ..Default::default()
    }))
    .unwrap();
    let mut telemetry = GpuTelemetry::new(&device).unwrap();
    telemetry.submit(
        &device,
        &queue,
        device.create_command_encoder(&Default::default()),
        "test submission",
    );
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let report = telemetry.finish(&queue);
    assert_eq!(report.submissions, 1);
    assert_eq!(report.samples.len(), 1);
    assert_eq!(report.samples[0].label, "test submission");
    assert!(report.samples[0].elapsed_ms.is_finite());
    assert!(report.samples[0].elapsed_ms >= 0.0);
    assert!(telemetry.finish(&queue).samples.is_empty());
}
