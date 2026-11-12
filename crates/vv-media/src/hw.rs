//! Hardware decoding through FFmpeg's hwaccels: one path for every backend,
//! the frames downloaded to system memory. See plans/HW_DECODE.md.

use ffmpeg::ffi;
use ffmpeg_next as ffmpeg;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HwDevice {
    VideoToolbox,
    Cuda,
    /// `None`: the driver's first device; else the GPU whose name contains
    /// the string (FFmpeg matches it against `deviceName`).
    Vulkan(Option<String>),
    D3d11va,
}

impl HwDevice {
    fn device_type(&self) -> ffi::AVHWDeviceType {
        match self {
            Self::VideoToolbox => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
            Self::Cuda => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
            Self::Vulkan(_) => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN,
            Self::D3d11va => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
        }
    }
}

struct DeviceRef(*mut ffi::AVBufferRef);

// SAFETY: a device context is refcounted and meant to be shared by decoders
// on any thread; this reference is never freed nor written.
unsafe impl Send for DeviceRef {}

/// One device per `HwDevice` per process; `None` if it could not be created.
static DEVICES: LazyLock<Mutex<HashMap<HwDevice, Option<DeviceRef>>>> =
    LazyLock::new(Default::default);

/// Bytes of HW surfaces all decoders may hold together (see `Lease`).
static BUDGET: AtomicUsize = AtomicUsize::new(usize::MAX);
static USED: AtomicUsize = AtomicUsize::new(0);

pub fn set_budget_bytes(bytes: usize) {
    BUDGET.store(bytes, Ordering::Relaxed);
}

/// How much of the budget a decoder may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwPriority {
    Normal,
    /// Only up to half of the budget, so it cannot crowd out the `Normal`
    /// ones opened later (the window behind the playhead, proxies).
    Low,
}

/// A decoder's share of `BUDGET`, given back on drop.
pub(crate) struct Lease(usize);

impl Lease {
    pub(crate) fn take(bytes: usize, priority: HwPriority) -> Option<Self> {
        let budget = BUDGET.load(Ordering::Relaxed);
        let limit = match priority {
            HwPriority::Normal => budget,
            HwPriority::Low => budget / 2,
        };
        USED.try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
            (used.saturating_add(bytes) <= limit).then_some(used + bytes)
        })
        .ok()
        .map(|_| Self(bytes))
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        USED.fetch_sub(self.0, Ordering::AcqRel);
    }
}

/// Surfaces a decoder of this stream allocates: the largest DPB (16, HEVC
/// and H.264), `extra_hw_frames` and the one being decoded.
pub(crate) fn surfaces_bytes(width: u32, height: u32, format: ffi::AVPixelFormat) -> usize {
    // SAFETY: a static table, null for an unknown format.
    let deep = unsafe {
        let desc = ffi::av_pix_fmt_desc_get(format);
        !desc.is_null() && (*desc).comp[0].depth > 8
    };
    let frame = crate::decode::yuv420_frame_bytes(width, height) * if deep { 2 } else { 1 };
    frame * (16 + EXTRA_HW_FRAMES as usize + 1)
}

const EXTRA_HW_FRAMES: i32 = 2;

/// Media whose HW decode failed: they open in software from then on.
static FAILED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);

/// A new reference to the device, created on first use.
fn device_ref(device: &HwDevice) -> Option<*mut ffi::AVBufferRef> {
    crate::probe::ensure_init();
    let mut devices = DEVICES.lock().unwrap();
    let created = devices.entry(device.clone()).or_insert_with(|| {
        let name = match device {
            HwDevice::Vulkan(Some(name)) => std::ffi::CString::new(name.as_str()).ok(),
            _ => None,
        };
        let mut buf = std::ptr::null_mut();
        // SAFETY: `buf` receives a new reference on success only.
        let ret = unsafe {
            ffi::av_hwdevice_ctx_create(
                &mut buf,
                device.device_type(),
                name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
                std::ptr::null_mut(),
                0,
            )
        };
        if ret < 0 {
            return None;
        }
        // SAFETY: a device just created, of `device`'s type.
        if !unsafe { can_decode(buf) } {
            unsafe { ffi::av_buffer_unref(&mut buf) };
            return None;
        }
        Some(DeviceRef(buf))
    });
    // SAFETY: the stored reference stays valid for the whole process.
    created.as_ref().map(|d| unsafe { ffi::av_buffer_ref(d.0) })
}

/// A Vulkan device opens without video decode queues too (Asahi's
/// Honeykrisp): each stream would then fail and fall back one by one.
///
/// # Safety
/// `buf` must reference a valid `AVHWDeviceContext`.
unsafe fn can_decode(buf: *mut ffi::AVBufferRef) -> bool {
    let ctx = unsafe { &*((*buf).data as *const ffi::AVHWDeviceContext) };
    if ctx.type_ != ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN {
        return true;
    }
    let vk = unsafe { &*(ctx.hwctx as *const ffi::AVVulkanDeviceContext) };
    (0..vk.nb_enabled_dev_extensions as usize).any(|i| {
        let name = unsafe { std::ffi::CStr::from_ptr(*vk.enabled_dev_extensions.add(i)) };
        name == c"VK_KHR_video_decode_queue"
    })
}

/// Whether opening `device` was tried and failed, without waiting for a
/// creation in progress.
pub fn known_unavailable(device: &HwDevice) -> bool {
    DEVICES
        .try_lock()
        .is_ok_and(|devices| matches!(devices.get(device), Some(None)))
}

/// Whether `device` can be opened. Creating a CUDA device costs
/// 100–300 ms: worth calling from a background thread at startup.
pub fn available(device: &HwDevice) -> bool {
    device_ref(device).is_some_and(|mut buf| {
        // SAFETY: the reference just taken.
        unsafe { ffi::av_buffer_unref(&mut buf) };
        true
    })
}

pub(crate) fn has_failed(path: &Path) -> bool {
    FAILED.lock().unwrap().contains(path)
}

pub(crate) fn mark_failed(path: &Path) {
    FAILED.lock().unwrap().insert(path.to_path_buf());
}

/// A codec context set up for a hwaccel, before `avcodec_open2`.
#[derive(Clone, Debug)]
pub(crate) struct HwState {
    /// The format of the frames while the hwaccel works: any other means
    /// FFmpeg fell back to software on its own.
    pub pix_fmt: ffi::AVPixelFormat,
    pub device: HwDevice,
}

/// Sets up `ctx` for the first of `devices` that the codec supports and
/// that opens. FFmpeg's default `get_format` picks the HW format of the
/// device in `hw_device_ctx`, and a software one if the hwaccel refuses.
///
/// # Safety
/// `ctx` must be a valid decoder context not opened yet.
pub(crate) unsafe fn attach(
    ctx: *mut ffi::AVCodecContext,
    devices: &[HwDevice],
) -> Option<HwState> {
    let codec = unsafe { ffi::avcodec_find_decoder((*ctx).codec_id) };
    if codec.is_null() {
        return None;
    }
    for device in devices {
        let Some(pix_fmt) = hw_pix_fmt(codec, device.device_type()) else {
            continue;
        };
        let Some(buf) = device_ref(device) else {
            continue;
        };
        unsafe {
            (*ctx).hw_device_ctx = buf;
            // The decoder holds a frame (`Decoder::pending`) while it decodes on.
            (*ctx).extra_hw_frames = EXTRA_HW_FRAMES;
        }
        return Some(HwState {
            pix_fmt,
            device: device.clone(),
        });
    }
    None
}

fn hw_pix_fmt(
    codec: *const ffi::AVCodec,
    device_type: ffi::AVHWDeviceType,
) -> Option<ffi::AVPixelFormat> {
    for i in 0.. {
        // SAFETY: the configs end with a null.
        let config = unsafe { ffi::avcodec_get_hw_config(codec, i) };
        if config.is_null() {
            return None;
        }
        let config = unsafe { &*config };
        if config.methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0
            && config.device_type == device_type
        {
            return Some(config.pix_fmt);
        }
    }
    None
}

pub(crate) fn is_hw_frame(frame: &ffmpeg::frame::Video) -> bool {
    // SAFETY: plain field read.
    unsafe { !(*frame.as_ptr()).hw_frames_ctx.is_null() }
}

/// `frame` in system memory. VideoToolbox maps it (unified memory, no copy
/// beyond the one `pack_plane` does anyway); CUDA cannot, and on Vulkan
/// mapping measured slower than a transfer.
pub(crate) fn download(
    frame: &ffmpeg::frame::Video,
    spent: &mut Duration,
) -> Result<ffmpeg::frame::Video, crate::MediaError> {
    let start = Instant::now();
    let mut sw = ffmpeg::frame::Video::empty();
    // SAFETY: `sw` is blank, as both calls want it; on failure they leave
    // it blank.
    unsafe {
        let src = frame.as_ptr();
        let dst = sw.as_mut_ptr();
        let mappable = (*src).format == ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32;
        if !mappable || ffi::av_hwframe_map(dst, src, ffi::AV_HWFRAME_MAP_READ as i32) < 0 {
            let ret = ffi::av_hwframe_transfer_data(dst, src, 0);
            if ret < 0 {
                return Err(ffmpeg::Error::from(ret).into());
            }
            let ret = ffi::av_frame_copy_props(dst, src);
            if ret < 0 {
                return Err(ffmpeg::Error::from(ret).into());
            }
        }
    }
    *spent += start.elapsed();
    Ok(sw)
}
