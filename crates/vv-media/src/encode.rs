//! Encoding + muxing into MP4 (H.264 x264/NVENC/VideoToolbox/Vulkan + AAC). It receives already
//! composited I420 frames (BT.709 limited) and already mixed PCM; it only
//! converts the audio into the AAC encoder's native format.

use ffmpeg::codec::{self, encoder};
use ffmpeg::format::sample::{Sample, Type as SampleType};
use ffmpeg::format::{self, Pixel};
use ffmpeg::software::resampling::context::Context as Resampler;
use ffmpeg::{ChannelLayout, Dictionary};
use ffmpeg_next as ffmpeg;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    X264,
    Nvenc,
    VideoToolbox,
    Vulkan,
}

impl VideoCodec {
    pub const ALL: [Self; 4] = [Self::X264, Self::Nvenc, Self::VideoToolbox, Self::Vulkan];

    /// The encoders that can exist on this platform.
    pub fn choices() -> &'static [Self] {
        if cfg!(target_os = "macos") {
            &[Self::X264, Self::VideoToolbox]
        } else {
            &[Self::X264, Self::Nvenc, Self::Vulkan]
        }
    }

    fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::X264 => "libx264",
            Self::Nvenc => "h264_nvenc",
            Self::VideoToolbox => "h264_videotoolbox",
            Self::Vulkan => "h264_vulkan",
        }
    }

    /// From fastest to slowest.
    pub fn presets(self) -> &'static [&'static str] {
        match self {
            Self::X264 => &[
                "ultrafast",
                "superfast",
                "veryfast",
                "faster",
                "fast",
                "medium",
                "slow",
                "slower",
                "veryslow",
            ],
            Self::Nvenc => &["p1", "p2", "p3", "p4", "p5", "p6", "p7"],
            Self::VideoToolbox | Self::Vulkan => &[],
        }
    }

    pub fn default_preset(self) -> &'static str {
        match self {
            // At equal CRF, quality similar to "medium", ~3× faster and
            // ~2× larger files.
            Self::X264 => "superfast",
            Self::Nvenc => "p5",
            Self::VideoToolbox | Self::Vulkan => "",
        }
    }

    /// The scale of `VideoSettings::quality`.
    pub fn quality_range(self) -> std::ops::RangeInclusive<u8> {
        match self {
            Self::X264 | Self::Nvenc | Self::Vulkan => 0..=51,
            Self::VideoToolbox => 1..=100,
        }
    }

    /// Whether a higher `quality` means a better picture: VideoToolbox's
    /// scale goes up, the quantizers of the others down.
    pub fn quality_rises(self) -> bool {
        self == Self::VideoToolbox
    }

    pub fn default_quality(self) -> u8 {
        match self {
            Self::X264 | Self::Nvenc => 20,
            // A constant QP spends more than a CRF at the same number.
            Self::Vulkan => 23,
            Self::VideoToolbox => 65,
        }
    }

    /// Tries to actually encode a few frames (result cached): NVENC is
    /// compiled into many ffmpeg builds even where there is no NVIDIA GPU,
    /// a Vulkan driver may have no encode queue, VideoToolbox has no
    /// constant quality on Intel Macs, and the first open can cost a second.
    /// An encoder that opens but writes wrong colors is left out too: ANV's
    /// experimental encode on Gen9 drops the Cr plane.
    pub fn is_available(self) -> bool {
        *AVAILABLE[self as usize].get_or_init(|| {
            crate::probe::ensure_init();
            match self.check_colors() {
                Ok(None) => true,
                Ok(Some(got)) => {
                    eprintln!(
                        "{} encodes {CHECK_COLOR:?} as {got:?}: not offered, the export stays on the CPU",
                        self.ffmpeg_name()
                    );
                    false
                }
                Err(_) => false,
            }
        })
    }

    /// Like `is_available` without blocking: the first call starts the check
    /// on a thread, `None` until it is done.
    pub fn availability(self) -> Option<bool> {
        static STARTED: [AtomicBool; 4] = [const { AtomicBool::new(false) }; 4];
        if let Some(&available) = AVAILABLE[self as usize].get() {
            return Some(available);
        }
        if !STARTED[self as usize].swap(true, Ordering::Relaxed) {
            std::thread::spawn(move || self.is_available());
        }
        None
    }

    /// Encodes and decodes back a solid `CHECK_COLOR`: the YUV it came back
    /// as if it is not the same.
    fn check_colors(self) -> Result<Option<[u8; 3]>, crate::MediaError> {
        const SIZE: usize = 256;
        let settings = VideoSettings::for_codec(self);
        let path = std::env::temp_dir().join(format!(
            "venturi-encoder-check-{}-{self:?}.mp4",
            std::process::id()
        ));
        let checked = (|| {
            let fps = vv_core::Rational::new(25, 1);
            let mut encoder = Encoder::new(&path, SIZE as u32, SIZE as u32, fps, &settings, None)?;
            let frame = solid_i420(SIZE, SIZE, CHECK_COLOR);
            for _ in 0..5 {
                encoder.write_video_frame(&frame)?;
            }
            encoder.finish()?;
            let mut decoder = crate::decode::Decoder::open(&path)?;
            let (_, decoded) = decoder.next_frame()?.ok_or_else(|| {
                crate::MediaError::NoStream(format!("{} wrote no frame", self.ffmpeg_name()))
            })?;
            let (u, v) = decoded.chroma_at(SIZE / 4, SIZE / 4);
            let got = [decoded.y[SIZE / 2 * SIZE + SIZE / 2], u, v];
            let same = got
                .iter()
                .zip(CHECK_COLOR)
                .all(|(&g, w)| g.abs_diff(w) <= 4);
            Ok(if same { None } else { Some(got) })
        })();
        let _ = std::fs::remove_file(&path);
        checked
    }
}

static AVAILABLE: [OnceLock<bool>; 4] = [const { OnceLock::new() }; 4];

/// Red in BT.709 limited range: Y, U and V all far from each other and from
/// the neutral 128, so a swapped or missing chroma plane shows.
const CHECK_COLOR: [u8; 3] = [63, 102, 240];

/// A dense I420 frame of one color, as `Encoder::write_video_frame` takes it.
pub(crate) fn solid_i420(width: usize, height: usize, [y, u, v]: [u8; 3]) -> Vec<u8> {
    let chroma = width.div_ceil(2) * height.div_ceil(2);
    let mut data = vec![y; width * height];
    data.extend(std::iter::repeat_n(u, chroma));
    data.extend(std::iter::repeat_n(v, chroma));
    data
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoSettings {
    pub codec: VideoCodec,
    /// One of `codec.presets()`.
    pub preset: String,
    /// CRF for x264, CQ for NVENC: lower = higher quality.
    pub quality: u8,
}

impl VideoSettings {
    pub fn for_codec(codec: VideoCodec) -> Self {
        Self {
            codec,
            preset: codec.default_preset().into(),
            quality: codec.default_quality(),
        }
    }
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self::for_codec(VideoCodec::X264)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    FdkAac,
}

impl AudioCodec {
    pub const ALL: [Self; 2] = [Self::Aac, Self::FdkAac];

    fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::FdkAac => "libfdk_aac",
        }
    }

    pub fn is_available(self) -> bool {
        crate::probe::ensure_init();
        encoder::find_by_name(self.ffmpeg_name()).is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSettings {
    pub codec: AudioCodec,
    pub bitrate_kbps: u32,
    /// Only for `AudioCodec::Aac`: "fast" coder instead of "twoloop", much
    /// quicker in stereo at slightly lower quality.
    pub fast_coder: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            codec: AudioCodec::Aac,
            bitrate_kbps: 128,
            fast_coder: false,
        }
    }
}

struct VideoState {
    encoder: encoder::Video,
    stream_index: usize,
    /// Encoder time base (1/fps: one pts unit = one frame), different from
    /// the output stream's (`ost_time_base`), onto which the packets must be
    /// rescaled before writing them (MP4 muxer).
    time_base: ffmpeg::Rational,
    ost_time_base: ffmpeg::Rational,
    next_pts: i64,
    /// Staging frame of the encoders that take GPU frames (Vulkan): they
    /// accept NV12, not three-plane I420.
    nv12: Option<ffmpeg::frame::Video>,
}

struct AudioState {
    encoder: encoder::Audio,
    /// F32 packed -> AAC encoder format, same rate (resampling is already
    /// done upstream).
    resampler: Resampler,
    input_layout: ChannelLayout,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    ost_time_base: ffmpeg::Rational,
    /// Samples per channel the AAC encoder expects in each frame
    /// (fixed for the native `aac` encoder, not variable frame size).
    frame_size: usize,
    channels: u16,
    rate: u32,
    /// Interleaved samples accumulated while waiting to reach a multiple
    /// of `frame_size * channels`.
    pending: Vec<f32>,
    next_pts: i64,
}

/// Video encoder + optional audio + muxer, one frame at a time. One per
/// export.
pub struct Encoder {
    octx: format::context::Output,
    video: VideoState,
    audio: Option<AudioState>,
}

impl Encoder {
    /// `audio`: `Some((sample_rate, channels, settings))` if the project
    /// has an audio track to export, `None` for a video-only export.
    pub fn new(
        path: &Path,
        width: u32,
        height: u32,
        fps: vv_core::Rational,
        video: &VideoSettings,
        audio: Option<(u32, u16, &AudioSettings)>,
    ) -> Result<Self, crate::MediaError> {
        crate::probe::ensure_init();

        let mut octx = format::output(path)?;
        let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);

        let (video_encoder, nv12) = open_video_encoder(video, width, height, fps, global_header)?;
        let video_time_base = video_encoder.time_base();
        let mut video_ost = octx.add_stream(video_encoder.codec())?;
        let video_stream_index = video_ost.index();
        video_ost.set_parameters(&video_encoder);

        // --- audio: AAC (optional) ---
        let audio_state = match audio {
            Some((sample_rate, channels, settings)) => {
                let audio_codec =
                    encoder::find_by_name(settings.codec.ffmpeg_name()).ok_or_else(|| {
                        crate::MediaError::NoStream(format!(
                            "encoder {} not available",
                            settings.codec.ffmpeg_name()
                        ))
                    })?;
                let audio_ost = octx.add_stream(audio_codec)?;
                let audio_stream_index = audio_ost.index();

                let input_layout = if channels >= 2 {
                    ChannelLayout::STEREO
                } else {
                    ChannelLayout::MONO
                };

                let mut audio_ctx = codec::context::Context::new_with_codec(audio_codec)
                    .encoder()
                    .audio()?;
                audio_ctx.set_rate(sample_rate as i32);
                audio_ctx.set_bit_rate(settings.bitrate_kbps as usize * 1000);
                audio_ctx.set_channel_layout(input_layout);
                audio_ctx.set_format(
                    audio_codec
                        .audio()?
                        .formats()
                        .and_then(|mut formats| formats.next())
                        .ok_or_else(|| {
                            crate::MediaError::NoStream(
                                "no sample format supported by the AAC encoder".into(),
                            )
                        })?,
                );
                let audio_time_base = ffmpeg::Rational::new(1, sample_rate as i32);
                audio_ctx.set_time_base(audio_time_base);
                if global_header {
                    audio_ctx.set_flags(codec::Flags::GLOBAL_HEADER);
                }
                let mut audio_opts = Dictionary::new();
                if settings.codec == AudioCodec::Aac {
                    let coder = if settings.fast_coder {
                        "fast"
                    } else {
                        "twoloop"
                    };
                    audio_opts.set("aac_coder", coder);
                }
                let audio_encoder = audio_ctx.open_as_with(audio_codec, audio_opts)?;
                let mut audio_ost = audio_ost;
                audio_ost.set_parameters(&audio_encoder);

                let out_format = audio_encoder.format();
                let out_layout = audio_encoder.channel_layout();
                let resampler = Resampler::get(
                    Sample::F32(SampleType::Packed),
                    input_layout,
                    sample_rate,
                    out_format,
                    out_layout,
                    sample_rate,
                )?;
                let frame_size = (audio_encoder.frame_size() as usize).max(1);

                Some(AudioState {
                    encoder: audio_encoder,
                    resampler,
                    input_layout,
                    stream_index: audio_stream_index,
                    time_base: audio_time_base,
                    // Updated after `write_header`.
                    ost_time_base: audio_time_base,
                    frame_size,
                    channels,
                    rate: sample_rate,
                    pending: Vec::new(),
                    next_pts: 0,
                })
            }
            None => None,
        };

        octx.write_header()?;

        // The muxer may normalize the time_base declared during
        // `write_header`: the *effective* one of the output stream must be
        // read back from there, not assumed equal to the encoder's.
        let video_ost_time_base = octx.stream(video_stream_index).unwrap().time_base();
        let video = VideoState {
            encoder: video_encoder,
            stream_index: video_stream_index,
            time_base: video_time_base,
            ost_time_base: video_ost_time_base,
            next_pts: 0,
            nv12,
        };

        let audio = audio_state.map(|mut state| {
            state.ost_time_base = octx.stream(state.stream_index).unwrap().time_base();
            state
        });

        Ok(Self { octx, video, audio })
    }

    /// `i420`: dense and consecutive Y, U, V planes, chroma `(w+1)/2 x (h+1)/2`
    /// (the format of `vv_render::Compositor::render_layers_i420`) — one
    /// video frame of output advanced.
    pub fn write_video_frame(&mut self, i420: &[u8]) -> Result<(), crate::MediaError> {
        let video = &mut self.video;
        let width = video.encoder.width() as usize;
        let height = video.encoder.height() as usize;
        let chroma_width = width.div_ceil(2);
        let (luma, chroma) = i420.split_at(width * height);
        let (u, v) = chroma.split_at(chroma.len() / 2);
        let mut frame = match &mut video.nv12 {
            Some(nv12) => {
                fill_plane(nv12, 0, luma, width);
                interleave_chroma(nv12, u, v, chroma_width);
                upload(&video.encoder, nv12)?
            }
            None => {
                let mut yuv =
                    ffmpeg::frame::Video::new(Pixel::YUV420P, width as u32, height as u32);
                fill_plane(&mut yuv, 0, luma, width);
                fill_plane(&mut yuv, 1, u, chroma_width);
                fill_plane(&mut yuv, 2, v, chroma_width);
                yuv
            }
        };
        frame.set_pts(Some(video.next_pts));
        frame.set_kind(ffmpeg::picture::Type::None);
        video.encoder.send_frame(&frame)?;
        video.next_pts += 1;
        drain_packets(
            &mut self.octx,
            &mut video.encoder,
            video.stream_index,
            video.time_base,
            video.ost_time_base,
        )
    }
    /// `samples`: interleaved f32 PCM, already at the sample rate/channels declared
    /// in `new`. Buffers internally at AAC frame boundaries
    /// (`frame_size`); no-op if the project has no audio.
    pub fn write_audio_samples(&mut self, samples: &[f32]) -> Result<(), crate::MediaError> {
        let Self { octx, audio, .. } = self;
        let Some(audio) = audio else {
            return Ok(());
        };
        // Never `drain` from the head of `pending` block by block: with all
        // the export audio at once it becomes quadratic (minutes).
        let mut pending = std::mem::take(&mut audio.pending);
        pending.extend_from_slice(samples);
        let frame_len = audio.frame_size * audio.channels as usize;
        let mut chunks = pending.chunks_exact(frame_len);
        for chunk in &mut chunks {
            write_audio_chunk(octx, audio, chunk)?;
        }
        audio.pending = chunks.remainder().to_vec();
        Ok(())
    }

    /// Flushes the encoders and `write_trailer`. The last audio block must be
    /// padded with silence: native AAC only accepts frames of `frame_size`.
    pub fn finish(mut self) -> Result<(), crate::MediaError> {
        self.video.encoder.send_eof()?;
        let video = &mut self.video;
        drain_packets(
            &mut self.octx,
            &mut video.encoder,
            video.stream_index,
            video.time_base,
            video.ost_time_base,
        )?;

        if let Some(mut audio) = self.audio.take() {
            if !audio.pending.is_empty() {
                let frame_len = audio.frame_size * audio.channels as usize;
                audio.pending.resize(frame_len, 0.0);
                let chunk = std::mem::take(&mut audio.pending);
                write_audio_chunk(&mut self.octx, &mut audio, &chunk)?;
            }
            audio.encoder.send_eof()?;
            drain_packets(
                &mut self.octx,
                &mut audio.encoder,
                audio.stream_index,
                audio.time_base,
                audio.ost_time_base,
            )?;
        }

        self.octx.write_trailer()?;
        Ok(())
    }
}

fn write_audio_chunk(
    octx: &mut format::context::Output,
    audio: &mut AudioState,
    samples: &[f32],
) -> Result<(), crate::MediaError> {
    let channels = audio.channels as usize;
    let frame_len = samples.len() / channels.max(1);
    let mut src = ffmpeg::frame::Audio::new(
        Sample::F32(SampleType::Packed),
        frame_len,
        audio.input_layout,
    );
    src.set_rate(audio.rate);
    {
        let (chunks, _) = src.data_mut(0).as_chunks_mut::<4>();
        for (bytes, &sample) in chunks.iter_mut().zip(samples.iter()) {
            *bytes = sample.to_ne_bytes();
        }
    }

    let mut resampled = ffmpeg::frame::Audio::empty();
    audio.resampler.run(&src, &mut resampled)?;
    resampled.set_pts(Some(audio.next_pts));
    audio.next_pts += frame_len as i64;

    audio.encoder.send_frame(&resampled)?;
    drain_packets(
        octx,
        &mut audio.encoder,
        audio.stream_index,
        audio.time_base,
        audio.ost_time_base,
    )
}

/// The video encoder of `settings`, opened, and the NV12 staging frame if
/// it takes GPU frames.
fn open_video_encoder(
    settings: &VideoSettings,
    width: u32,
    height: u32,
    fps: vv_core::Rational,
    global_header: bool,
) -> Result<(encoder::Video, Option<ffmpeg::frame::Video>), crate::MediaError> {
    let name = settings.codec.ffmpeg_name();
    let codec = encoder::find_by_name(name)
        .ok_or_else(|| crate::MediaError::NoStream(format!("encoder {name} not available")))?;
    let mut ctx = codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()?;
    ctx.set_width(width);
    ctx.set_height(height);
    ctx.set_format(Pixel::YUV420P);
    ctx.set_time_base(ffmpeg::Rational::new(fps.den, fps.num));
    ctx.set_frame_rate(Some(ffmpeg::Rational::new(fps.num, fps.den)));
    ctx.set_colorspace(ffmpeg::color::Space::BT709);
    ctx.set_color_range(ffmpeg::color::Range::MPEG);
    // ffmpeg-next has no setter for primaries/trc.
    unsafe {
        let raw = ctx.as_mut_ptr();
        (*raw).color_primaries = ffmpeg::ffi::AVColorPrimaries::AVCOL_PRI_BT709;
        (*raw).color_trc = ffmpeg::ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
    }
    let mut flags = codec::Flags::empty();
    if global_header {
        flags |= codec::Flags::GLOBAL_HEADER;
    }
    let mut opts = Dictionary::new();
    if !settings.codec.presets().is_empty() {
        opts.set("preset", &settings.preset);
    }
    let quality = settings.quality.to_string();
    let mut nv12 = None;
    match settings.codec {
        VideoCodec::X264 => opts.set("crf", &quality),
        VideoCodec::Nvenc => {
            // Without bit_rate at 0, NVENC ignores `cq` and stays on the
            // context's default bitrate.
            ctx.set_bit_rate(0);
            opts.set("rc", "vbr");
            opts.set("cq", &quality);
        }
        VideoCodec::VideoToolbox => {
            // `-q:v`: constant quality, Apple Silicon only.
            flags |= codec::Flags::QSCALE;
            ctx.set_global_quality(settings.quality as i32 * ffmpeg::ffi::FF_QP2LAMBDA);
        }
        VideoCodec::Vulkan => {
            // SAFETY: a context not opened yet.
            unsafe { attach_vulkan_frames(ctx.as_mut_ptr())? };
            opts.set("rc_mode", "cqp");
            opts.set("qp", &quality);
            nv12 = Some(ffmpeg::frame::Video::new(Pixel::NV12, width, height));
        }
    }
    ctx.set_flags(flags);
    Ok((ctx.open_with(opts)?, nv12))
}

/// Gives `ctx` a pool of NV12 frames on the first Vulkan device.
///
/// # Safety
/// `ctx` must be a valid encoder context not opened yet, with its size set.
unsafe fn attach_vulkan_frames(
    ctx: *mut ffmpeg::ffi::AVCodecContext,
) -> Result<(), crate::MediaError> {
    use ffmpeg::ffi;
    let check = |ret: i32| {
        if ret < 0 {
            Err(crate::MediaError::from(ffmpeg::Error::from(ret)))
        } else {
            Ok(())
        }
    };
    unsafe {
        let mut device = std::ptr::null_mut();
        check(ffi::av_hwdevice_ctx_create(
            &mut device,
            ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        ))?;
        let mut frames = ffi::av_hwframe_ctx_alloc(device);
        // The frames context holds its own reference to the device.
        ffi::av_buffer_unref(&mut device);
        if frames.is_null() {
            return Err(ffmpeg::Error::Other {
                errno: ffmpeg::error::ENOMEM,
            }
            .into());
        }
        let frames_ctx = &mut *((*frames).data as *mut ffi::AVHWFramesContext);
        frames_ctx.format = ffi::AVPixelFormat::AV_PIX_FMT_VULKAN;
        frames_ctx.sw_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
        frames_ctx.width = (*ctx).width;
        frames_ctx.height = (*ctx).height;
        if let Err(e) = check(ffi::av_hwframe_ctx_init(frames)) {
            ffi::av_buffer_unref(&mut frames);
            return Err(e);
        }
        // Freed with the context.
        (*ctx).hw_frames_ctx = frames;
        (*ctx).pix_fmt = ffi::AVPixelFormat::AV_PIX_FMT_VULKAN;
    }
    Ok(())
}

/// `nv12` copied into a frame of `encoder`'s GPU pool.
fn upload(
    encoder: &encoder::Video,
    nv12: &ffmpeg::frame::Video,
) -> Result<ffmpeg::frame::Video, crate::MediaError> {
    use ffmpeg::ffi;
    let mut gpu = ffmpeg::frame::Video::empty();
    // SAFETY: the encoder was opened with `hw_frames_ctx`
    // (`attach_vulkan_frames`), and `gpu` is blank as both calls want it.
    unsafe {
        let frames = (*encoder.as_ptr()).hw_frames_ctx;
        let ret = ffi::av_hwframe_get_buffer(frames, gpu.as_mut_ptr(), 0);
        if ret < 0 {
            return Err(ffmpeg::Error::from(ret).into());
        }
        let ret = ffi::av_hwframe_transfer_data(gpu.as_mut_ptr(), nv12.as_ptr(), 0);
        if ret < 0 {
            return Err(ffmpeg::Error::from(ret).into());
        }
    }
    Ok(gpu)
}

/// Writes the dense `u` and `v` planes (rows of `row_width` samples) as
/// NV12's interleaved chroma plane.
fn interleave_chroma(frame: &mut ffmpeg::frame::Video, u: &[u8], v: &[u8], row_width: usize) {
    let stride = frame.stride(1);
    let data = frame.data_mut(1);
    let rows = u
        .chunks_exact(row_width.max(1))
        .zip(v.chunks_exact(row_width.max(1)));
    for (y, (u_row, v_row)) in rows.enumerate() {
        let dst = &mut data[y * stride..y * stride + 2 * row_width];
        let (pairs, _) = dst.as_chunks_mut::<2>();
        for (pair, (&u, &v)) in pairs.iter_mut().zip(u_row.iter().zip(v_row)) {
            *pair = [u, v];
        }
    }
}

/// Writes the packets already ready from `encoder` into the muxer.
pub(crate) fn drain_packets(
    octx: &mut format::context::Output,
    encoder: &mut encoder::Encoder,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    ost_time_base: ffmpeg::Rational,
) -> Result<(), crate::MediaError> {
    let mut packet = ffmpeg::Packet::empty();
    while encoder.receive_packet(&mut packet).is_ok() {
        packet.set_stream(stream_index);
        packet.rescale_ts(time_base, ost_time_base);
        packet.write_interleaved(octx)?;
    }
    Ok(())
}

/// Copies a dense plane (rows of `row_width` bytes) into a plane of `frame`,
/// which may have a wider stride.
pub(crate) fn fill_plane(
    frame: &mut ffmpeg::frame::Video,
    plane: usize,
    src: &[u8],
    row_width: usize,
) {
    let stride = frame.stride(plane);
    let data = frame.data_mut(plane);
    if stride == row_width {
        data[..src.len()].copy_from_slice(src);
        return;
    }
    for (y, row) in src.chunks_exact(row_width.max(1)).enumerate() {
        data[y * stride..y * stride + row.len()].copy_from_slice(row);
    }
}

#[cfg(test)]
#[path = "tests/encode.rs"]
mod tests;
