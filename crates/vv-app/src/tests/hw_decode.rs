use super::*;

fn gpu(name: &str, vendor: u32, device_type: wgpu::DeviceType) -> wgpu::AdapterInfo {
    wgpu::AdapterInfo {
        name: name.into(),
        vendor,
        ..wgpu::AdapterInfo::new(device_type, wgpu::Backend::Vulkan)
    }
}

fn intel() -> wgpu::AdapterInfo {
    gpu(
        "Intel(R) UHD Graphics 620",
        0x8086,
        wgpu::DeviceType::IntegratedGpu,
    )
}

fn nvidia() -> wgpu::AdapterInfo {
    gpu(
        "NVIDIA GeForce RTX 2070 SUPER",
        NVIDIA_VENDOR_ID,
        wgpu::DeviceType::DiscreteGpu,
    )
}

fn lavapipe() -> wgpu::AdapterInfo {
    gpu(
        "llvmpipe (LLVM 20.1.8, 256 bits)",
        0x10005,
        wgpu::DeviceType::Cpu,
    )
}

fn vulkan(gpu: &wgpu::AdapterInfo) -> HwDevice {
    HwDevice::Vulkan(Some(gpu.name.clone()))
}

#[test]
fn auto_on_a_hybrid_laptop_decodes_on_the_integrated_gpu() {
    let devices = resolve(HwDecodeMode::Auto, &[nvidia(), intel(), lavapipe()], false);
    assert_eq!(devices, [vulkan(&intel())]);
}

#[test]
fn auto_on_an_nvidia_desktop_prefers_nvdec_then_vulkan() {
    let devices = resolve(HwDecodeMode::Auto, &[lavapipe(), nvidia()], false);
    assert_eq!(devices, [HwDevice::Cuda, vulkan(&nvidia())]);
}

#[test]
fn auto_without_nvidia_goes_through_vulkan_only() {
    let amd = gpu(
        "AMD Radeon RX 7600 (RADV NAVI33)",
        0x1002,
        wgpu::DeviceType::DiscreteGpu,
    );
    let devices = resolve(HwDecodeMode::Auto, std::slice::from_ref(&amd), false);
    assert_eq!(devices, [vulkan(&amd)]);
}

#[test]
fn a_software_vulkan_driver_is_never_a_decoder() {
    assert!(resolve(HwDecodeMode::Auto, &[lavapipe()], false).is_empty());
    assert!(resolve(HwDecodeMode::Vulkan, &[lavapipe()], false).is_empty());
}

#[test]
fn explicit_choices_override_the_hybrid_rule() {
    let gpus = [intel(), nvidia()];
    assert_eq!(resolve(HwDecodeMode::Nvdec, &gpus, false), [HwDevice::Cuda]);
    assert_eq!(
        resolve(HwDecodeMode::Vulkan, &gpus, false),
        [vulkan(&nvidia())]
    );
    assert!(resolve(HwDecodeMode::Off, &gpus, false).is_empty());
}

#[test]
fn macos_decodes_on_videotoolbox_unless_off() {
    assert_eq!(
        resolve(HwDecodeMode::Auto, &[], true),
        [HwDevice::VideoToolbox]
    );
    assert!(resolve(HwDecodeMode::Off, &[], true).is_empty());
}

#[test]
fn modes_round_trip_through_their_ids() {
    for mode in [
        HwDecodeMode::Auto,
        HwDecodeMode::Off,
        HwDecodeMode::Nvdec,
        HwDecodeMode::Vulkan,
        HwDecodeMode::D3d11va,
    ] {
        assert_eq!(HwDecodeMode::from_id(mode.id()), Some(mode));
    }
}

#[test]
fn device_labels_name_the_backend_and_the_gpu() {
    assert_eq!(device_label(&HwDevice::VideoToolbox), "VideoToolbox");
    assert_eq!(device_label(&HwDevice::Cuda), "NVDEC");
    assert_eq!(
        device_label(&vulkan(&intel())),
        "Vulkan, Intel(R) UHD Graphics 620"
    );
}

#[test]
fn windows_auto_tries_d3d11va_after_nvdec_and_before_vulkan() {
    let nvidia_desktop = resolve(HwDecodeMode::Auto, &[nvidia()], false);
    assert_eq!(
        with_d3d11va(nvidia_desktop),
        [HwDevice::Cuda, HwDevice::D3d11va, vulkan(&nvidia())]
    );
    let hybrid = resolve(HwDecodeMode::Auto, &[nvidia(), intel()], false);
    assert_eq!(with_d3d11va(hybrid), [HwDevice::D3d11va, vulkan(&intel())]);
    assert_eq!(with_d3d11va(Vec::new()), [HwDevice::D3d11va]);
}
