//! GPU compositor (wgpu) and title rasterization, see
//! ARCHITECTURE.md § GPU compositing.

pub mod compositor;
pub mod scopes;
mod smooth_cut;
pub mod text;

pub use compositor::{
    Compositor, Layer, LayerContent, OutputFrame, PooledTexture, YuvChroma, YuvFrame,
    device_limits, fit_output_size,
};
pub use scopes::{ScopeKind, Scopes};
/// Re-exported: whoever owns a texture for `LayerContent::Texture` must use the
/// same wgpu version as the compositor.
pub use wgpu;

/// `WGPU_BACKEND` wins; otherwise DX12 on Windows, where some Intel
/// Vulkan drivers crash natively in `request_device`.
pub fn backends() -> wgpu::Backends {
    wgpu::Backends::from_env().unwrap_or(if cfg!(windows) {
        wgpu::Backends::DX12 | wgpu::Backends::GL
    } else {
        wgpu::Backends::PRIMARY | wgpu::Backends::GL
    })
}

/// The Vulkan adapters of the machine, without opening them.
pub fn vulkan_adapters() -> Vec<wgpu::AdapterInfo> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
        .iter()
        .map(|adapter| adapter.get_info())
        .collect()
}
