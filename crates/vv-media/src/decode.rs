//! Decoding a media: `next_frame` in sequence or after `seek_to_time`.
//! Every pixel format becomes dense 8-bit YUV420P or NV12 — YUVA420P if the
//! source has an alpha channel (PNG, WebM with alpha, ProRes 4444), so
//! transparency makes it all the way to compositing; the conversion to RGB
//! is done by the shader. Matrix and range are read from the original frame
//! (scaling does not change them), with a resolution heuristic if missing.

use ffmpeg::format::Pixel;
use ffmpeg::software::scaling::{context::Context as Scaler, flag::Flags};
use ffmpeg::util::color;
use ffmpeg_next as ffmpeg;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use vv_core::FrameIdx;

pub use vv_core::ColorMatrix;

/// The 4:2:0 chroma of a `FrameYuv420`.
#[derive(Clone)]
pub enum Chroma {
    Planar {
        u: Vec<u8>,
        v: Vec<u8>,
    },
    /// NV12, as hwaccels hand it out: U and V alternating in one plane,
    /// `2 * chroma_width` bytes a row. Carried to the shader as it is, since
    /// making it planar would cost as much as the scaling `Decoder` avoids.
    Interleaved(Vec<u8>),
}

/// 8-bit YUV 4:2:0 frame, dense planes (no row padding).
#[derive(Clone)]
pub struct FrameYuv420 {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub chroma: Chroma,
    /// Chroma samples per row and rows: `plane_width(1)` x `plane_height(1)`
    /// of the scaled frame (ffmpeg rounds up on odd dimensions, not a plain
    /// `width/2`).
    pub chroma_width: u32,
    pub chroma_height: u32,
    pub matrix: ColorMatrix,
    /// `true` = full range (0-255), `false` = limited (16-235/240), the norm.
    pub full_range: bool,
    /// Per-pixel coverage (`width`x`height`, not subsampled, *straight*
    /// non-premultiplied alpha): `Some` when the source format has an alpha
    /// channel (`format_has_alpha`), `None` when it is opaque. A PNG with
    /// transparency carries it all the way here, and without it its
    /// transparent areas would show the color the file left underneath —
    /// often black — instead of letting the clip below show through.
    pub alpha: Option<Vec<u8>>,
}

impl FrameYuv420 {
    /// Bytes used by the planes.
    pub fn byte_len(&self) -> usize {
        let chroma = match &self.chroma {
            Chroma::Planar { u, v } => u.len() + v.len(),
            Chroma::Interleaved(uv) => uv.len(),
        };
        self.y.len() + chroma + self.alpha.as_ref().map_or(0, Vec::len)
    }

    /// U and V of the chroma sample at `(x, y)`, in chroma coordinates.
    pub fn chroma_at(&self, x: usize, y: usize) -> (u8, u8) {
        let i = y * self.chroma_width as usize + x;
        match &self.chroma {
            Chroma::Planar { u, v } => (u[i], v[i]),
            Chroma::Interleaved(uv) => (uv[2 * i], uv[2 * i + 1]),
        }
    }
}

/// Bytes of an 8-bit YUV420 `width`x`height` frame, to estimate cache
/// budgets before having decoded it.
pub fn yuv420_frame_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * 3 / 2
}

/// Matrix when the source does not signal it: BT.601 below 720 rows,
/// BT.709 above, like ffmpeg. BT.2020 is never guessed.
fn guess_matrix(space: color::Space, height: u32) -> ColorMatrix {
    match space {
        color::Space::BT709 => ColorMatrix::Bt709,
        color::Space::BT2020NCL | color::Space::BT2020CL => ColorMatrix::Bt2020,
        // Same BT.601 matrix. The others (rare) fall back on the heuristic.
        color::Space::SMPTE170M | color::Space::BT470BG => ColorMatrix::Bt601,
        _ => {
            if height >= 720 {
                ColorMatrix::Bt709
            } else {
                ColorMatrix::Bt601
            }
        }
    }
}

/// Decoder open on a single video stream of a media. Not `Sync`: every
/// decode thread (e.g. the decode-ahead in `playback`) owns its own
/// instance.
pub struct Decoder {
    path: PathBuf,
    ictx: ffmpeg::format::context::Input,
    decoder: ffmpeg::codec::decoder::Video,
    /// Built from the first frame that needs one: the format the decoder
    /// declares before decoding is not always the one of its frames.
    scaler: Option<Scaler>,
    /// See `set_max_size`.
    max_size: Option<(u32, u32)>,
    video_stream_index: usize,
    time_base: ffmpeg::Rational,
    fps: vv_core::Rational,
    /// After `send_eof` ffmpeg refuses another one before a flush: it only
    /// drains.
    eof_sent: bool,
    /// Frame decoded by `seek_to_time` to check where it landed:
    /// `next_frame` returns it first. Not converted yet: it may be skipped.
    pending: Option<(FrameIdx, ffmpeg::frame::Video)>,
    /// See `skip_before`.
    skip_before: Option<FrameIdx>,
    skipping_nonref: bool,
    /// The single frame of an image: seek and `next_frame` always return it,
    /// the rest of the decode would treat EOF after one frame as an error.
    still_image: Option<Arc<FrameYuv420>>,
    /// Index growing on every `next_frame` on an image, as in a video.
    synthetic_idx: FrameIdx,
    /// Next index to hand out. The decoder emits a contiguous CFR sequence:
    /// a VFR source (phone screen capture) leaves holes between one pts and
    /// the next, and whoever caches by index would wait forever for frames
    /// the stream never produces.
    emit_idx: Option<FrameIdx>,
    /// Last emitted frame, repeated to fill those holes. Shared, not
    /// copied: a still stretch of a screen capture costs one frame however
    /// many CFR slots it spans.
    held: Option<Arc<FrameYuv420>>,
    hw: Option<crate::hw::HwState>,
    _hw_lease: Option<crate::hw::Lease>,
    transfer_time: Duration,
    #[cfg(test)]
    fail_after: Option<u32>,
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self, crate::MediaError> {
        Self::open_with(path, &[], crate::hw::HwPriority::Normal)
    }

    /// Like `open`, decoding on the first of `hw` that the codec supports.
    /// Software if none does, if the media already failed on HW, if its
    /// surfaces do not fit the budget (`hw::set_budget_bytes`), or if the
    /// hwaccel fails on a frame (see `decode_next_raw`).
    pub fn open_with(
        path: &Path,
        hw: &[crate::hw::HwDevice],
        priority: crate::hw::HwPriority,
    ) -> Result<Self, crate::MediaError> {
        crate::probe::ensure_init();

        let ictx = ffmpeg::format::input(&path)?;
        let video_stream = crate::probe::video_stream(&ictx)
            .ok_or_else(|| crate::MediaError::NoStream(path.display().to_string()))?;
        let video_stream_index = video_stream.index();
        let time_base = video_stream.time_base();
        let fps = crate::probe::media_fps(path, &video_stream);

        let mut decoder_ctx =
            ffmpeg::codec::context::Context::from_parameters(video_stream.parameters())?.decoder();
        let lease = if hw.is_empty() || crate::hw::has_failed(path) {
            None
        } else {
            // SAFETY: plain fields, filled from the stream parameters.
            let bytes = unsafe {
                let ctx = decoder_ctx.as_ptr();
                crate::hw::surfaces_bytes((*ctx).width as u32, (*ctx).height as u32, (*ctx).pix_fmt)
            };
            crate::hw::Lease::take(bytes, priority)
        };
        let hw = lease
            .as_ref()
            // SAFETY: the context is not opened yet.
            .and_then(|_| unsafe { crate::hw::attach(decoder_ctx.as_mut_ptr(), hw) });
        let lease = lease.filter(|_| hw.is_some());
        // Multithreaded decode: a seek decodes from the last keyframe up to
        // the target, in parallel it is much faster.
        decoder_ctx.set_threading(ffmpeg::threading::Config {
            kind: ffmpeg::threading::Type::Frame,
            count: if hw.is_some() { 1 } else { 0 },
        });
        let decoder = decoder_ctx.video()?;

        Ok(Self {
            path: path.to_path_buf(),
            ictx,
            decoder,
            scaler: None,
            max_size: None,
            video_stream_index,
            time_base,
            fps,
            eof_sent: false,
            pending: None,
            skip_before: None,
            skipping_nonref: false,
            still_image: None,
            synthetic_idx: 0,
            emit_idx: None,
            held: None,
            hw,
            _hw_lease: lease,
            transfer_time: Duration::ZERO,
            #[cfg(test)]
            fail_after: None,
        })
    }

    /// Like `open`, for a still image: decodes the single frame immediately.
    pub fn open_image(path: &Path) -> Result<Self, crate::MediaError> {
        let mut decoder = Self::open(path)?;
        // The same fps as `probe_image`, not the demuxer's (fictitious) one, or
        // the seek seconds would not match the media frames.
        decoder.fps = crate::probe::IMAGE_FPS;
        let (_, mut raw) = decoder
            .decode_next_raw()?
            .ok_or_else(|| crate::MediaError::NoStream(path.display().to_string()))?;
        decoder.still_image = Some(decoder.convert(&mut raw)?);
        Ok(decoder)
    }

    pub fn width(&self) -> u32 {
        self.decoder.width()
    }

    pub fn height(&self) -> u32 {
        self.decoder.height()
    }

    pub fn fps(&self) -> vv_core::Rational {
        self.fps
    }

    /// Whether the frames come from a hwaccel. Can turn `false` on any
    /// decode, never back.
    pub fn is_hw(&self) -> bool {
        self.hw.is_some()
    }

    /// The device decoding now; `None` on the CPU.
    pub fn hw_device(&self) -> Option<&crate::hw::HwDevice> {
        self.hw.as_ref().map(|hw| &hw.device)
    }

    /// Time spent so far bringing HW frames to system memory.
    pub fn transfer_time(&self) -> Duration {
        self.transfer_time
    }

    /// Seek to the keyframe `<= secs`, guaranteed: on mp4 with B-frames
    /// `avformat_seek_file` sometimes lands on the keyframe *after*, and the
    /// frames in between would never be decoded. The landing frame is decoded
    /// immediately (kept in `pending`) and, if it is past the target, the seek
    /// is retried further back, doubling the step.
    pub fn seek_to_time(&mut self, secs: f64) -> Result<(), crate::MediaError> {
        let target_idx = (secs.max(0.0) * self.fps.as_f64()).round() as FrameIdx;
        if self.still_image.is_some() {
            // An image lands exactly on the target.
            self.synthetic_idx = target_idx;
            return Ok(());
        }
        self.skip_before = None;
        self.set_skip_nonref(false);
        let start_ts = (secs * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64;
        let mut ts = start_ts;
        // Initial backoff step: one second, doubled on every attempt.
        let mut step = i64::from(ffmpeg::ffi::AV_TIME_BASE);
        const MAX_RETRIES: u32 = 20;
        for _ in 0..MAX_RETRIES {
            self.ictx.seek(ts, ..ts)?;
            self.decoder.flush();
            self.eof_sent = false;
            self.pending = None;
            self.emit_idx = None;
            self.held = None;
            let landed = self.decode_next_raw_unchecked();
            if self.hw_failed(&landed) {
                self.reopen_in_software()?;
                ts = start_ts;
                step = i64::from(ffmpeg::ffi::AV_TIME_BASE);
                continue;
            }
            match landed? {
                Some((idx, frame)) if idx > target_idx && ts > 0 => {
                    ts = ts.saturating_sub(step).max(0);
                    step = step.saturating_mul(2);
                    let _ = frame; // discarded, retried further back
                }
                landed => {
                    self.pending = landed;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Frame the last `seek_to_time` landed on (its keyframe), until
    /// `next_frame` hands it out.
    pub fn landing(&self) -> Option<FrameIdx> {
        self.pending.as_ref().map(|(idx, _)| *idx)
    }

    /// The keyframe at or before `idx`, decoded alone: its packet is sent and
    /// the stream drained at once, so frame threading does not first decode
    /// as many frames as it has threads. Leaves the decoder with nothing in
    /// flight: `next_frame` needs a seek after it.
    pub fn keyframe_before(
        &mut self,
        idx: FrameIdx,
    ) -> Result<Option<(FrameIdx, Arc<FrameYuv420>)>, crate::MediaError> {
        if let Some(frame) = self.still_image.clone() {
            return Ok(Some((idx, frame)));
        }
        self.skip_before = None;
        self.set_skip_nonref(false);
        self.pending = None;
        self.emit_idx = None;
        self.held = None;
        let secs = idx.max(0) as f64 / self.fps.as_f64().max(1e-9);
        let mut ts = (secs * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64;
        // As in `seek_to_time`: a seek may land past the target, retried
        // further back.
        let mut step = i64::from(ffmpeg::ffi::AV_TIME_BASE);
        for _ in 0..20 {
            self.ictx.seek(ts, ..ts)?;
            self.decoder.flush();
            self.eof_sent = false;
            let key = loop {
                let mut packet = ffmpeg::Packet::empty();
                match packet.read(&mut self.ictx) {
                    Ok(()) if packet.stream() == self.video_stream_index && packet.is_key() => {
                        break Some(packet);
                    }
                    Ok(()) => {}
                    Err(ffmpeg::Error::Eof) => break None,
                    Err(e) => return Err(e.into()),
                }
            };
            let Some(packet) = key else {
                return Ok(None);
            };
            if packet.pts().is_some_and(|pts| self.frame_idx(pts) > idx) && ts > 0 {
                ts = ts.saturating_sub(step).max(0);
                step = step.saturating_mul(2);
                continue;
            }
            self.decoder.send_packet(&packet)?;
            self.decoder.send_eof()?;
            let mut decoded = ffmpeg::frame::Video::empty();
            let received = self.decoder.receive_frame(&mut decoded);
            self.decoder.flush();
            if received.is_err() {
                return Ok(None);
            }
            let (key_idx, mut raw) = self.raw_frame(decoded);
            return Ok(Some((key_idx, self.convert(&mut raw)?)));
        }
        Ok(None)
    }

    /// The frame `landing` names, converted, without handing it out: it stays
    /// the next one `next_frame` gives or a skip drops. Costs no decoding, the
    /// seek already did it.
    pub fn landing_frame(
        &mut self,
    ) -> Result<Option<(FrameIdx, Arc<FrameYuv420>)>, crate::MediaError> {
        if let Some(frame) = self.still_image.clone() {
            return Ok(Some((self.synthetic_idx, frame)));
        }
        let Some((idx, mut raw)) = self.pending.take() else {
            return Ok(None);
        };
        let converted = self.convert(&mut raw);
        self.pending = Some((idx, raw));
        Ok(Some((idx, converted?)))
    }

    /// After a seek: the frames before `idx` are wanted by nobody, only
    /// decoded as far as later frames reference them. `next_frame` skips them
    /// without converting them, and the non-reference ones (most B-frames)
    /// are not even decoded. Lasts until the first frame handed out.
    pub fn skip_before(&mut self, idx: FrameIdx) {
        if self.still_image.is_none() {
            self.skip_before = Some(idx);
        }
    }

    pub fn is_skipping(&self) -> bool {
        self.skip_before.is_some()
    }

    /// Advances a `skip_before` by at most `max` frames, so a caller can
    /// check between steps whether it still wants the frame it is heading to.
    /// Returns the last frame skipped.
    pub fn skip_some(&mut self, max: usize) -> Result<Option<FrameIdx>, crate::MediaError> {
        let mut last_skipped = None;
        for _ in 0..max {
            let Some(skip_before) = self.skip_before else {
                break;
            };
            if self.pending.is_none() {
                self.pending = self.decode_next_raw()?;
            }
            match self.landing() {
                Some(idx) if idx < skip_before => {
                    self.pending = None;
                    last_skipped = Some(idx);
                }
                // A frame to hand out, or the end of the stream. The skipped
                // stretch is no hole to fill with the last frame handed out.
                _ => {
                    self.skip_before = None;
                    self.set_skip_nonref(false);
                    self.emit_idx = None;
                    self.held = None;
                }
            }
        }
        Ok(last_skipped)
    }

    fn set_skip_nonref(&mut self, skip: bool) {
        if skip == self.skipping_nonref {
            return;
        }
        self.skipping_nonref = skip;
        // SAFETY: plain field of the open codec context, read by libavcodec
        // on every packet sent afterwards.
        unsafe {
            (*self.decoder.as_mut_ptr()).skip_frame = if skip {
                ffmpeg::ffi::AVDiscard::AVDISCARD_NONREF
            } else {
                ffmpeg::ffi::AVDiscard::AVDISCARD_DEFAULT
            };
        }
    }

    fn frame_idx(&self, pts: i64) -> FrameIdx {
        let secs =
            pts as f64 * self.time_base.numerator() as f64 / self.time_base.denominator() as f64;
        (secs * self.fps.as_f64()).round() as FrameIdx
    }

    /// Decodes the next available video frame in presentation order.
    /// `Ok(None)` at the end of the stream.
    pub fn next_frame(
        &mut self,
    ) -> Result<Option<(FrameIdx, Arc<FrameYuv420>)>, crate::MediaError> {
        if let Some(frame) = self.still_image.clone() {
            // "Advances" by a synthetic frame, always the same image — see the
            // docs of `still_image`/`synthetic_idx`.
            let idx = self.synthetic_idx;
            self.synthetic_idx += 1;
            return Ok(Some((idx, frame)));
        }
        self.skip_some(usize::MAX)?;
        loop {
            if self.pending.is_none() {
                self.pending = self.decode_next_raw()?;
            }
            let Some(decoded_idx) = self.landing() else {
                return Ok(None);
            };
            let emit = *self.emit_idx.get_or_insert(decoded_idx);
            if decoded_idx == emit {
                let (_, mut raw) = self.pending.take().unwrap();
                let frame = self.convert(&mut raw)?;
                self.emit_idx = Some(emit + 1);
                self.held = Some(frame.clone());
                return Ok(Some((emit, frame)));
            }
            if decoded_idx < emit {
                // Burst denser than the declared fps: its slot is already
                // gone. Dropping it keeps the index tied to the pts —
                // letting it through instead would drift the sequence past
                // the real duration and break sync with the audio.
                self.pending = None;
                continue;
            }
            match &self.held {
                Some(held) => {
                    let frame = held.clone();
                    self.emit_idx = Some(emit + 1);
                    return Ok(Some((emit, frame)));
                }
                // Nothing to repeat yet (first frame after a seek): start the
                // sequence where the stream actually is.
                None => self.emit_idx = Some(decoded_idx),
            }
        }
    }

    /// The next decoded frame in presentation order, not converted yet. If
    /// the hwaccel fails, it goes on in software from where it was.
    fn decode_next_raw(
        &mut self,
    ) -> Result<Option<(FrameIdx, ffmpeg::frame::Video)>, crate::MediaError> {
        let result = self.decode_next_raw_unchecked();
        if !self.hw_failed(&result) {
            return result;
        }
        let skip_before = self.skip_before;
        let emit_idx = self.emit_idx;
        let held = self.held.take();
        self.reopen_in_software()?;
        let Some(resume) = skip_before.or(emit_idx) else {
            // Nothing handed out since the open: the new decoder is there too.
            return self.decode_next_raw();
        };
        self.seek_to_time(resume as f64 / self.fps.as_f64())?;
        // Resumed from the keyframe: `next_frame` drops what is before
        // `emit_idx`, `skip_some` what is before `skip_before`.
        self.skip_before = skip_before;
        self.emit_idx = emit_idx;
        self.held = held;
        Ok(self.pending.take())
    }

    /// An error, or a frame in a format other than the hwaccel's: FFmpeg
    /// falls back to software silently when the hwaccel refuses the stream,
    /// and with frame threading off that would be a single-thread decode.
    fn hw_failed(
        &self,
        result: &Result<Option<(FrameIdx, ffmpeg::frame::Video)>, crate::MediaError>,
    ) -> bool {
        let Some(hw) = &self.hw else {
            return false;
        };
        match result {
            Err(_) => true,
            // SAFETY: plain field read.
            Ok(Some((_, frame))) => unsafe { (*frame.as_ptr()).format != hw.pix_fmt as i32 },
            Ok(None) => false,
        }
    }

    fn reopen_in_software(&mut self) -> Result<(), crate::MediaError> {
        eprintln!(
            "HW decoding failed on {}: software from now on",
            self.path.display()
        );
        crate::hw::mark_failed(&self.path);
        let transfer_time = self.transfer_time;
        *self = Self::open(&self.path)?;
        self.transfer_time = transfer_time;
        Ok(())
    }

    fn decode_next_raw_unchecked(
        &mut self,
    ) -> Result<Option<(FrameIdx, ffmpeg::frame::Video)>, crate::MediaError> {
        #[cfg(test)]
        if let Some(n) = self.fail_after.as_mut() {
            if *n == 0 {
                self.fail_after = None;
                return Err(ffmpeg::Error::InvalidData.into());
            }
            *n -= 1;
        }
        let mut decoded = ffmpeg::frame::Video::empty();

        // EOF already sent: only the remaining frames are drained.
        if self.eof_sent {
            return Ok(if self.decoder.receive_frame(&mut decoded).is_ok() {
                Some(self.raw_frame(decoded))
            } else {
                None
            });
        }

        loop {
            // `Packet::read` does not unref the previous packet: reusing one
            // would leak every packet read.
            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut self.ictx) {
                Ok(()) => {
                    if packet.stream() != self.video_stream_index {
                        continue;
                    }
                    // A non-reference frame is needed only by itself: before
                    // `skip_before` it can go undecoded. Without a pts, decode it.
                    let skip = self.skip_before.is_some_and(|skip_before| {
                        packet
                            .pts()
                            .is_some_and(|pts| self.frame_idx(pts) < skip_before)
                    });
                    self.set_skip_nonref(skip);
                    self.decoder.send_packet(&packet)?;
                    if self.decoder.receive_frame(&mut decoded).is_ok() {
                        return Ok(Some(self.raw_frame(decoded)));
                    }
                }
                Err(ffmpeg::Error::Eof) => {
                    self.decoder.send_eof()?;
                    self.eof_sent = true;
                    if self.decoder.receive_frame(&mut decoded).is_ok() {
                        return Ok(Some(self.raw_frame(decoded)));
                    }
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn raw_frame(&self, decoded: ffmpeg::frame::Video) -> (FrameIdx, ffmpeg::frame::Video) {
        (self.frame_idx(decoded.pts().unwrap_or(0)), decoded)
    }

    /// Frames larger than `max` come out scaled down to fit it, with their
    /// aspect: a reduced preview uploads and caches less. Not for an image,
    /// decoded at `open_image`.
    pub fn set_max_size(&mut self, max: Option<(u32, u32)>) {
        self.max_size = max;
    }

    fn convert(
        &mut self,
        decoded: &mut ffmpeg::frame::Video,
    ) -> Result<Arc<FrameYuv420>, crate::MediaError> {
        let mut downloaded;
        let decoded = if crate::hw::is_hw_frame(decoded) {
            downloaded = crate::hw::download(decoded, &mut self.transfer_time)?;
            &mut downloaded
        } else {
            decoded
        };
        let matrix = guess_matrix(decoded.color_space(), decoded.height());
        let format = decoded.format();
        // An RGB frame always declares itself `JPEG`, but that is the range of
        // the RGB, not of the YUV sws derives from it: there it converts to
        // limited unless told otherwise, and believing it full would wash out
        // every imported image.
        let full_range = !format_is_rgb(format)
            && (decoded.color_range() == color::Range::JPEG
                || without_deprecated_range(format) != format);
        decoded.set_format(without_deprecated_range(format));
        let frame =
            yuv420_from_decoded(&mut self.scaler, decoded, matrix, full_range, self.max_size)?;
        Ok(Arc::new(frame))
    }
}

/// The flags of `format`: ffmpeg-next does not expose
/// `AVPixFmtDescriptor::flags`, and `nb_components` alone would not
/// tell an alpha channel from the padding of an `0RGB`.
fn format_flags(format: Pixel) -> u64 {
    match format.descriptor() {
        Some(descriptor) => unsafe { (*descriptor.as_ptr()).flags },
        None => 0,
    }
}

/// `true` if `format` carries a real alpha channel (PNG, WebM with alpha,
/// ProRes 4444) and not a fourth padding channel.
fn format_has_alpha(format: Pixel) -> bool {
    format_flags(format) & ffmpeg::ffi::AV_PIX_FMT_FLAG_ALPHA as u64 != 0
}

/// `true` for RGB formats (an imported PNG, say), which the scaler must
/// convert to YUV instead of just rearranging the planes.
fn format_is_rgb(format: Pixel) -> bool {
    format_flags(format) & ffmpeg::ffi::AV_PIX_FMT_FLAG_RGB as u64 != 0
}

/// The format to scale to: YUVA420P preserves the alpha in the fourth plane,
/// YUV420P would throw it away. Semi-planar sources (hwaccel frames) stay
/// semi-planar, the 10-bit ones only lose their bits.
fn target_format(source: Pixel) -> Pixel {
    if format_has_alpha(source) {
        Pixel::YUVA420P
    } else if matches!(source, Pixel::NV12 | Pixel::P010LE | Pixel::P016LE) {
        Pixel::NV12
    } else {
        Pixel::YUV420P
    }
}

/// The `yuvj*` formats make sws compress the full range into limited, but
/// the range is already carried by `FrameYuv420::full_range`: the conversion
/// treats them as `yuv*`, leaving the values intact.
fn without_deprecated_range(format: Pixel) -> Pixel {
    match format {
        Pixel::YUVJ420P => Pixel::YUV420P,
        Pixel::YUVJ422P => Pixel::YUV422P,
        Pixel::YUVJ444P => Pixel::YUV444P,
        Pixel::YUVJ440P => Pixel::YUV440P,
        Pixel::YUVJ411P => Pixel::YUV411P,
        other => other,
    }
}

/// `(width, height)` scaled down to fit `max` with its aspect, even for
/// 4:2:0; unchanged if it already fits.
pub(crate) fn fitted_size((width, height): (u32, u32), max: Option<(u32, u32)>) -> (u32, u32) {
    let Some((max_w, max_h)) = max else {
        return (width, height);
    };
    if width <= max_w && height <= max_h {
        return (width, height);
    }
    let factor = (max_w as f64 / width as f64).min(max_h as f64 / height as f64);
    let even = |v: u32| ((v as f64 * factor / 2.0).round() as u32 * 2).max(2);
    (even(width), even(height))
}

/// The scaler converting `frame` to its target format and `size`, rebuilt
/// when either side changes.
fn scaler_for<'a>(
    scaler: &'a mut Option<Scaler>,
    frame: &ffmpeg::frame::Video,
    size: (u32, u32),
) -> Result<&'a mut Scaler, crate::MediaError> {
    let fits = scaler.as_ref().is_some_and(|s| {
        let (input, output) = (s.input(), s.output());
        input.format == frame.format()
            && input.width == frame.width()
            && input.height == frame.height()
            && (output.width, output.height) == size
    });
    if !fits {
        *scaler = Some(Scaler::get(
            frame.format(),
            frame.width(),
            frame.height(),
            target_format(frame.format()),
            size.0,
            size.1,
            Flags::BILINEAR,
        )?);
    }
    Ok(scaler.as_mut().unwrap())
}

fn yuv420_from_decoded(
    scaler: &mut Option<Scaler>,
    decoded: &ffmpeg::frame::Video,
    matrix: ColorMatrix,
    full_range: bool,
    max_size: Option<(u32, u32)>,
) -> Result<FrameYuv420, crate::MediaError> {
    let size = fitted_size((decoded.width(), decoded.height()), max_size);
    // Already in the target format and size (most camera and screen
    // recordings): sws would only copy the planes once more, on the thread
    // feeding the decoder.
    let converted;
    let scaled = if decoded.format() == target_format(decoded.format())
        && size == (decoded.width(), decoded.height())
    {
        decoded
    } else {
        let mut frame = ffmpeg::frame::Video::empty();
        scaler_for(scaler, decoded, size)?.run(decoded, &mut frame)?;
        converted = frame;
        &converted
    };

    let interleaved = scaled.format() == Pixel::NV12;
    // Rows may carry padding (decoder or sws): each plane is recompacted.
    let pack_plane = |index: usize| -> Vec<u8> {
        let samples = if interleaved && index == 1 { 2 } else { 1 };
        let w = scaled.plane_width(index) as usize * samples;
        let h = scaled.plane_height(index) as usize;
        let stride = scaled.stride(index);
        let plane = scaled.data(index);
        if stride == w {
            return plane[..w * h].to_vec();
        }
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            let start = row * stride;
            out.extend_from_slice(&plane[start..start + w]);
        }
        out
    };

    let width = scaled.width();
    let height = scaled.height();
    let y = pack_plane(0);
    let chroma = if interleaved {
        Chroma::Interleaved(pack_plane(1))
    } else {
        Chroma::Planar {
            u: pack_plane(1),
            v: pack_plane(2),
        }
    };
    let alpha = (scaled.format() == Pixel::YUVA420P).then(|| pack_plane(3));

    Ok(FrameYuv420 {
        width,
        height,
        y,
        chroma,
        chroma_width: scaled.plane_width(1),
        chroma_height: scaled.plane_height(1),
        matrix,
        full_range,
        alpha,
    })
}

#[cfg(test)]
#[path = "tests/decode.rs"]
mod tests;
