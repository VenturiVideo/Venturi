//! Which GPU decodes video (Settings > Playback): the setting resolved to
//! the devices every decoder of the app tries, in order.

use std::sync::{LazyLock, Mutex, OnceLock, RwLock};
use vv_media::HwDevice;
use vv_render::wgpu;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HwDecodeMode {
    #[default]
    Auto,
    Off,
    Nvdec,
    Vulkan,
    D3d11va,
}

impl HwDecodeMode {
    /// The choices that exist on this platform: macOS has only VideoToolbox,
    /// which is what `Auto` picks there.
    pub fn available() -> &'static [Self] {
        if cfg!(target_os = "macos") {
            &[Self::Auto, Self::Off]
        } else if cfg!(windows) {
            &[
                Self::Auto,
                Self::Off,
                Self::Nvdec,
                Self::D3d11va,
                Self::Vulkan,
            ]
        } else {
            &[Self::Auto, Self::Off, Self::Nvdec, Self::Vulkan]
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
            Self::Nvdec => "nvdec",
            Self::Vulkan => "vulkan",
            Self::D3d11va => "d3d11va",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [
            Self::Auto,
            Self::Off,
            Self::Nvdec,
            Self::Vulkan,
            Self::D3d11va,
        ]
        .into_iter()
        .find(|mode| mode.id() == id)
    }
}

const NVIDIA_VENDOR_ID: u32 = 0x10de;
const INTEL_VENDOR_ID: u32 = 0x8086;

/// Mesa's ANV offers Vulkan video before Gen12 only behind this flag, read
/// when a Vulkan instance starts.
///
/// # Safety
/// No other thread may be running (`std::env::set_var`).
pub unsafe fn enable_intel_experimental_decode() {
    const FLAG: &str = "video-decode";
    let flags = match std::env::var("ANV_DEBUG") {
        Ok(flags) if flags.split(',').any(|flag| flag == FLAG) => return,
        Ok(flags) if !flags.is_empty() => format!("{flags},{FLAG}"),
        _ => FLAG.to_owned(),
    };
    unsafe { std::env::set_var("ANV_DEBUG", flags) };
}

/// `None` while `start` is still enumerating the GPUs.
pub fn has_intel_gpu() -> Option<bool> {
    GPUS.get()
        .map(|gpus| gpus.iter().any(|gpu| gpu.vendor == INTEL_VENDOR_ID))
}

/// The devices for `mode` on a machine with `gpus` (its Vulkan adapters).
pub fn resolve(mode: HwDecodeMode, gpus: &[wgpu::AdapterInfo], macos: bool) -> Vec<HwDevice> {
    if macos {
        return match mode {
            HwDecodeMode::Off => Vec::new(),
            _ => vec![HwDevice::VideoToolbox],
        };
    }
    let real: Vec<&wgpu::AdapterInfo> = gpus
        .iter()
        .filter(|gpu| {
            matches!(
                gpu.device_type,
                wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::DiscreteGpu
            )
        })
        .collect();
    let of_type = |device_type| real.iter().find(|gpu| gpu.device_type == device_type);
    let vulkan_on = |gpu: &&wgpu::AdapterInfo| HwDevice::Vulkan(Some(gpu.name.clone()));
    let has_nvidia = real.iter().any(|gpu| gpu.vendor == NVIDIA_VENDOR_ID);
    match mode {
        HwDecodeMode::Off => Vec::new(),
        HwDecodeMode::Nvdec => vec![HwDevice::Cuda],
        HwDecodeMode::D3d11va => vec![HwDevice::D3d11va],
        HwDecodeMode::Vulkan => of_type(wgpu::DeviceType::DiscreteGpu)
            .or(real.first())
            .map(vulkan_on)
            .into_iter()
            .collect(),
        HwDecodeMode::Auto => {
            let integrated = of_type(wgpu::DeviceType::IntegratedGpu);
            let discrete = of_type(wgpu::DeviceType::DiscreteGpu);
            if let (Some(integrated), Some(_)) = (integrated, discrete) {
                // Hybrid laptop: decoding on the discrete GPU would wake it.
                return vec![vulkan_on(integrated)];
            }
            has_nvidia
                .then_some(HwDevice::Cuda)
                .into_iter()
                .chain(real.first().map(vulkan_on))
                .collect()
        }
    }
}

/// `Auto` on Windows: D3D11VA right after NVDEC, ahead of Vulkan video,
/// which fewer Windows drivers decode with.
fn with_d3d11va(mut devices: Vec<HwDevice>) -> Vec<HwDevice> {
    let at = usize::from(devices.first() == Some(&HwDevice::Cuda));
    devices.insert(at, HwDevice::D3d11va);
    devices
}

fn resolve_here(mode: HwDecodeMode, gpus: &[wgpu::AdapterInfo]) -> Vec<HwDevice> {
    let devices = resolve(mode, gpus, cfg!(target_os = "macos"));
    if cfg!(windows) && mode == HwDecodeMode::Auto {
        with_d3d11va(devices)
    } else {
        devices
    }
}

static GPUS: OnceLock<Vec<wgpu::AdapterInfo>> = OnceLock::new();

fn gpus() -> &'static [wgpu::AdapterInfo] {
    GPUS.get_or_init(|| {
        if cfg!(target_os = "macos") {
            Vec::new()
        } else {
            vv_render::vulkan_adapters()
        }
    })
}

/// Every GPU decoder of this machine for the export window, the Playback
/// setting's first. Starts checking them in the background, so the ones that
/// do not open turn `known_unavailable` while the window is up.
pub fn export_choices() -> Vec<HwDevice> {
    let has_nvidia = gpus().iter().any(|gpu| gpu.vendor == NVIDIA_VENDOR_ID);
    let mut choices = devices();
    let candidates = [HwDecodeMode::Auto, HwDecodeMode::Vulkan]
        .into_iter()
        .flat_map(|mode| resolve_here(mode, gpus()))
        .chain(has_nvidia.then_some(HwDevice::Cuda));
    for device in candidates {
        if !choices.contains(&device) {
            choices.push(device);
        }
    }
    choices.retain(|device| !vv_media::hw::known_unavailable(device));
    let check = choices.clone();
    std::thread::spawn(move || {
        for device in &check {
            vv_media::hw::available(device);
        }
    });
    choices
}

/// What `Auto` decodes on here, for the settings: `None` while `start` is
/// still enumerating the GPUs, `Some(None)` if there is no GPU decoder.
pub fn auto_device() -> Option<Option<HwDevice>> {
    let macos = cfg!(target_os = "macos");
    let gpus = if macos { &[][..] } else { GPUS.get()? };
    Some(
        resolve_here(HwDecodeMode::Auto, gpus)
            .into_iter()
            .find(|device| !vv_media::hw::known_unavailable(device)),
    )
}

pub fn device_label(device: &HwDevice) -> String {
    match device {
        HwDevice::VideoToolbox => "VideoToolbox".into(),
        HwDevice::Cuda => "NVDEC".into(),
        HwDevice::Vulkan(Some(gpu)) => format!("Vulkan, {gpu}"),
        HwDevice::Vulkan(None) => "Vulkan".into(),
        HwDevice::D3d11va => "D3D11VA".into(),
    }
}

/// The setting read at startup; unset (tests) means software.
static STARTUP_MODE: OnceLock<HwDecodeMode> = OnceLock::new();

/// Resolved on first use, blocking the callers meanwhile: decoders opened
/// while `start` is still at work wait for it instead of staying software.
static DEVICES: LazyLock<RwLock<Vec<HwDevice>>> = LazyLock::new(|| {
    let mode = STARTUP_MODE.get().copied().unwrap_or(HwDecodeMode::Off);
    RwLock::new(created(mode))
});

/// Resolves `mode` off the UI thread: enumerating the GPUs and initializing
/// CUDA cost up to a few hundred ms.
pub fn start(mode: HwDecodeMode) {
    let _ = STARTUP_MODE.set(mode);
    std::thread::spawn(|| LazyLock::force(&DEVICES));
}

/// The devices of the current setting, for `Decoder::open_with`.
pub fn devices() -> Vec<HwDevice> {
    DEVICES.read().unwrap().clone()
}

pub fn apply(mode: HwDecodeMode) {
    let devices = created(mode);
    *DEVICES.write().unwrap() = devices;
}

/// A setting whose GPU decoders all failed to open, for the warning in
/// Settings.
#[derive(Clone, Debug)]
pub struct Fallback {
    pub mode: HwDecodeMode,
    pub devices: Vec<HwDevice>,
}

/// Kept when the app then switches to `Off`, so the warning stays.
static FALLBACK: Mutex<Option<Fallback>> = Mutex::new(None);

pub fn fallback() -> Option<Fallback> {
    FALLBACK.lock().unwrap().clone()
}

fn created(mode: HwDecodeMode) -> Vec<HwDevice> {
    let resolved = resolve_here(mode, gpus());
    let devices: Vec<HwDevice> = resolved
        .iter()
        .filter(|device| vv_media::hw::available(device))
        .cloned()
        .collect();
    let mut fallback = FALLBACK.lock().unwrap();
    if !resolved.is_empty() && devices.is_empty() {
        *fallback = Some(Fallback {
            mode,
            devices: resolved,
        });
    } else if mode != HwDecodeMode::Off {
        *fallback = None;
    }
    devices
}

/// Default budget for the HW surfaces: an eighth of the physical memory,
/// which on Apple Silicon is also the GPU's.
pub fn default_budget_bytes() -> usize {
    physical_memory_bytes().map_or(1_000_000_000, |bytes| bytes / 8)
}

#[cfg(unix)]
fn physical_memory_bytes() -> Option<usize> {
    // SAFETY: plain queries, -1 on failure.
    let (pages, page_size) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    (pages > 0 && page_size > 0).then(|| pages as usize * page_size as usize)
}

#[cfg(windows)]
fn physical_memory_bytes() -> Option<usize> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: a plain query into a struct with its size set.
    unsafe {
        let mut status: MEMORYSTATUSEX = std::mem::zeroed();
        status.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        (GlobalMemoryStatusEx(&mut status) != 0).then_some(status.ullTotalPhys as usize)
    }
}

#[cfg(test)]
#[path = "tests/hw_decode.rs"]
mod tests;
