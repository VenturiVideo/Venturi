//! GPU compositor (wgpu) and title rasterization, see
//! ARCHITECTURE.md § GPU compositing.

pub mod compositor;
pub mod scopes;
pub mod text;

pub use compositor::{
    Compositor, Layer, LayerContent, OutputFrame, PooledTexture, YuvChroma, YuvFrame,
    device_limits, fit_output_size,
};
pub use scopes::{ScopeKind, Scopes};
/// Re-exported: whoever owns a texture for `LayerContent::Texture` must use the
/// same wgpu version as the compositor.
pub use wgpu;

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
