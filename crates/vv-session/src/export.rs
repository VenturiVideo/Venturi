//! Export: walks the timeline and writes H.264+AAC with the same layers and
//! the same mix as the preview. Runs on a dedicated thread, on a
//! snapshot of the project.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use vv_core::{Clip, ClipId, FrameIdx, MediaId, Project, Timeline, TimelineId, TrackKind};

use vv_audio::mixer::{
    AudioSource, ClipAudio, MixSnapshot, PROJECT_SAMPLE_RATE, mix_range, remix_channels_into,
    timeline_frame_to_sample,
};

use crate::frame_provider::{
    FrameProvider, GpuCompounds, OwnedLayer, clips_decoded_at, media_source_frame, track_layers_at,
};

pub(crate) const PROJECT_CHANNELS: u16 = 2;
const RENDER_AHEAD_FRAMES: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportSettings {
    pub output_path: PathBuf,
    /// Percentage of the timeline resolution.
    pub scale_percent: u32,
    pub video: vv_media::VideoSettings,
    /// `None` = export without audio.
    pub audio: Option<vv_media::AudioSettings>,
    /// GPU decoders to try, in order; empty decodes on the CPU.
    pub hw_decode: Vec<vv_media::HwDevice>,
}

impl ExportSettings {
    pub const SCALE_CHOICES: [u32; 4] = [100, 75, 50, 25];

    pub fn new(output_path: PathBuf) -> Self {
        Self {
            output_path,
            scale_percent: 100,
            video: vv_media::VideoSettings::default(),
            audio: Some(vv_media::AudioSettings::default()),
            hw_decode: Vec::new(),
        }
    }

    /// Like `new`, but with the fastest encoders available on this
    /// machine (NVENC, FDK). `new` stays deterministic for the tests.
    /// Blocks on the NVENC check, up to a second the first time.
    pub fn preferred(output_path: PathBuf) -> Self {
        let mut settings = Self::with_preferred_audio(output_path);
        if vv_media::VideoCodec::Nvenc.is_available() {
            settings.video = vv_media::VideoSettings::for_codec(vv_media::VideoCodec::Nvenc);
        }
        settings
    }

    /// `preferred` but for the video encoder, left to x264.
    pub fn with_preferred_audio(output_path: PathBuf) -> Self {
        let mut settings = Self::new(output_path);
        if vv_media::AudioCodec::FdkAac.is_available()
            && let Some(audio) = &mut settings.audio
        {
            audio.codec = vv_media::AudioCodec::FdkAac;
        }
        settings
    }

    /// Reduced to even dimensions (required by the encoders' 4:2:0);
    /// at 100% it stays exactly the timeline's.
    pub fn output_size(&self, timeline_size: (u32, u32)) -> (u32, u32) {
        if self.scale_percent >= 100 {
            return timeline_size;
        }
        let scale = |v: u32| ((v * self.scale_percent / 100) & !1).max(2);
        (scale(timeline_size.0), scale(timeline_size.1))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportError {
    TimelineNotFound,
    MediaNotFound,
    Cancelled,
    /// Decoding, composition or encoding failed.
    Failed(String),
}

impl ExportError {
    fn failed(e: impl std::fmt::Display) -> Self {
        Self::Failed(e.to_string())
    }
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimelineNotFound => f.write_str("timeline not found"),
            Self::MediaNotFound => f.write_str("media not found"),
            Self::Cancelled => f.write_str("export cancelled"),
            Self::Failed(e) => f.write_str(e),
        }
    }
}

#[derive(Default)]
pub struct ExportProgress {
    pub current_frame: FrameIdx,
    pub total_frames: FrameIdx,
    pub done: bool,
    pub error: Option<ExportError>,
    /// Updated along with `current_frame`, not only at the end.
    pub elapsed: std::time::Duration,
    pub decode: StageStats,
    pub compose: StageStats,
    pub encode: StageStats,
    pub output_path: PathBuf,
    /// Decode paths used so far, `None` being the CPU. Still images are left
    /// out: decoded once, they say nothing about the speed.
    pub decoders: Vec<Option<vv_media::HwDevice>>,
    /// GPU adapter of the composition.
    pub compositor: Option<String>,
    pub encoder: Option<vv_media::VideoCodec>,
    /// Until the first frame is written.
    pub startup: Option<std::time::Duration>,
    /// After the last frame is written: encoder flush and audio tail.
    pub finalize: Option<std::time::Duration>,
}

impl ExportProgress {
    pub fn fps(&self) -> Option<f64> {
        rate(self.current_frame, self.elapsed)
    }

    /// Share of the elapsed time `stage` spent working; the rest it waited
    /// on the other stages.
    pub fn busy_share(&self, stage: &StageStats) -> Option<f64> {
        (!self.elapsed.is_zero())
            .then(|| (stage.busy.as_secs_f64() / self.elapsed.as_secs_f64()).min(1.0))
    }
}

/// Time a pipeline stage spent working, without the waits on the other
/// stages: its fps is how fast it would go alone, so the slowest one is the
/// bottleneck.
#[derive(Default, Clone, Copy)]
pub struct StageStats {
    pub frames: FrameIdx,
    pub busy: std::time::Duration,
}

impl StageStats {
    pub fn fps(&self) -> Option<f64> {
        rate(self.frames, self.busy)
    }

    fn add(&mut self, busy: std::time::Duration) {
        self.frames += 1;
        self.busy += busy;
    }
}

fn rate(frames: FrameIdx, time: std::time::Duration) -> Option<f64> {
    (frames > 0 && !time.is_zero()).then(|| frames as f64 / time.as_secs_f64())
}

/// Decoder kept open for the active video clip, with a seek/reopen only
/// when the clip changes (not a new decoder for every output frame:
/// every seek is a flush to a keyframe, too slow to do on every frame).
struct ActiveClipDecoder {
    decoder: vv_media::Decoder,
    /// A conformed clip asks for the same source frame on consecutive
    /// timeline frames, and the decoder does not go back.
    last: Option<(FrameIdx, Arc<vv_media::FrameYuv420>)>,
}

impl ActiveClipDecoder {
    fn open_for(
        path: &Path,
        target_source_frame: FrameIdx,
        is_image: bool,
        hw: &[vv_media::HwDevice],
    ) -> Result<Self, ExportError> {
        // `Decoder::open` on an image would hit EOF after the first frame.
        let mut decoder = if is_image {
            vv_media::Decoder::open_image(path)
        } else {
            vv_media::Decoder::open_with(path, hw, vv_media::HwPriority::Normal)
        }
        .map_err(ExportError::failed)?;
        let secs = target_source_frame as f64 / decoder.fps().as_f64().max(1e-9);
        decoder.seek_to_time(secs).map_err(ExportError::failed)?;
        let mut me = Self {
            decoder,
            last: None,
        };
        me.advance_to(target_source_frame)?;
        Ok(me)
    }

    /// Decodes forward up to `target`; if already reached it returns the last
    /// frame again. `None` at the end of the stream.
    fn advance_to(
        &mut self,
        target: FrameIdx,
    ) -> Result<Option<Arc<vv_media::FrameYuv420>>, ExportError> {
        if let Some((idx, frame)) = &self.last
            && *idx >= target
        {
            return Ok(Some(frame.clone()));
        }
        loop {
            match self.decoder.next_frame().map_err(ExportError::failed)? {
                Some((idx, frame)) if idx >= target => {
                    self.last = Some((idx, frame.clone()));
                    return Ok(Some(frame));
                }
                Some(_) => continue,
                None => return Ok(None),
            }
        }
    }
}

/// Decoders kept open from one frame to the next, reopened only when the
/// clip changes.
#[derive(Default)]
struct StreamingFrameProvider {
    /// One per clip: several tracks can be active on the same frame.
    /// Pruned by `retain_clips`.
    active: HashMap<ClipId, ActiveClipDecoder>,
    hw: Vec<vv_media::HwDevice>,
    /// As `ExportProgress::decoders`.
    used: Vec<Option<vv_media::HwDevice>>,
}

impl StreamingFrameProvider {
    fn retain_clips(&mut self, keep: &[ClipId]) {
        self.active.retain(|id, _| keep.contains(id));
    }
}

impl FrameProvider for StreamingFrameProvider {
    fn frame_for(
        &mut self,
        project: &Project,
        clip: &Clip,
        timeline_frame: FrameIdx,
    ) -> Result<Option<Arc<vv_media::FrameYuv420>>, ExportError> {
        let Some((media_id, source_frame)) = media_source_frame(clip, timeline_frame) else {
            self.active.remove(&clip.id);
            return Ok(None);
        };
        let item = project
            .media_pool
            .get(media_id)
            .ok_or(ExportError::MediaNotFound)?;

        // A compound clip is not decoded: `GpuCompounds` composes it.
        // Getting here means it stopped at the nesting limit —
        // no layer for this clip, as for a missing media.
        if item.compound.is_some() {
            return Ok(None);
        }

        let path = item.path.clone();
        let is_image = item.meta.is_image();

        let decoder = match self.active.entry(clip.id) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(ActiveClipDecoder::open_for(
                &path,
                source_frame,
                is_image,
                &self.hw,
            )?),
        };
        let frame = decoder.advance_to(source_frame)?;
        let device = decoder.decoder.hw_device();
        if !is_image && !self.used.iter().any(|used| used.as_ref() == device) {
            self.used.push(device.cloned());
        }
        Ok(frame)
    }
}

/// Exports the frames in `range` to `output_path`. Blocking.
pub fn export_timeline(
    project: &Project,
    timeline_id: TimelineId,
    settings: &ExportSettings,
    range: std::ops::Range<FrameIdx>,
    progress: &Mutex<ExportProgress>,
    cancel: &AtomicBool,
) -> Result<(), ExportError> {
    let started = std::time::Instant::now();
    let finish = || {
        let mut p = progress.lock().unwrap();
        let now = started.elapsed();
        if p.current_frame > 0 {
            p.finalize = Some(now.saturating_sub(p.elapsed));
        }
        p.elapsed = now;
        p.done = true;
    };
    let timeline = project
        .timelines
        .get(timeline_id)
        .ok_or(ExportError::TimelineNotFound)?;

    let range = range.start.max(0)..range.end.min(timeline.total_frames());
    let total_frames = (range.end - range.start).max(0);
    progress.lock().unwrap().total_frames = total_frames;
    if total_frames <= 0 {
        finish();
        return Ok(());
    }

    let audio_settings = settings.audio.as_ref().filter(|_| {
        timeline
            .tracks_of_kind(TrackKind::Audio)
            .any(|(_, t)| !t.clips.is_empty())
    });
    let has_audio_track = audio_settings.is_some();

    let (out_w, out_h) = settings.output_size(timeline.resolution);
    let mut encoder = vv_media::Encoder::new(
        &settings.output_path,
        out_w,
        out_h,
        timeline.fps,
        &settings.video,
        audio_settings.map(|a| (PROJECT_SAMPLE_RATE, PROJECT_CHANNELS, a)),
    )
    .map_err(ExportError::failed)?;

    // Decode, GPU composition and encode on three threads: in series each one
    // waited for the others and none saturated the machine.
    let (decoded_tx, decoded_rx) =
        std::sync::mpsc::sync_channel::<Result<Vec<OwnedLayer>, ExportError>>(RENDER_AHEAD_FRAMES);
    let (composed_tx, composed_rx) =
        std::sync::mpsc::sync_channel::<Result<Vec<u8>, ExportError>>(RENDER_AHEAD_FRAMES);
    let resolution = timeline.resolution;
    let output = vv_render::OutputFrame::scaled(out_w, out_h, resolution);
    // A single one for the two GPU stages: the texture of a compound clip is born
    // in the decode stage and sampled in the composition one,
    // so they must be on the same device. Headless, so as not to contend
    // with the UI's.
    let compositor = vv_render::Compositor::new_headless_with_precision(project.precision);
    {
        let mut p = progress.lock().unwrap();
        p.output_path = settings.output_path.clone();
        p.compositor = Some(compositor.adapter_name().to_owned());
        p.encoder = Some(settings.video.codec);
    }
    let compositor = &compositor;
    std::thread::scope(|scope| {
        let mut audio_mix = has_audio_track.then(|| {
            let range = range.clone();
            scope.spawn(move || mix_audio_track(project, timeline, range))
        });

        let decode_range = range.clone();
        scope.spawn(move || {
            let mut provider = StreamingFrameProvider {
                hw: settings.hw_decode.clone(),
                ..Default::default()
            };
            for frame in decode_range {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let t = std::time::Instant::now();
                let decoded = decode_video_frame(
                    project,
                    timeline,
                    &mut provider,
                    compositor,
                    frame,
                    resolution,
                );
                let mut p = progress.lock().unwrap();
                p.decode.add(t.elapsed());
                if p.decoders != provider.used {
                    p.decoders.clone_from(&provider.used);
                }
                drop(p);
                let failed = decoded.is_err();
                // `send` fails only if the next stage has already stopped.
                if decoded_tx.send(decoded).is_err() || failed {
                    return;
                }
            }
        });

        scope.spawn(move || {
            for decoded in decoded_rx {
                let t = std::time::Instant::now();
                let composed =
                    decoded.map(|layers| compose_video_frame(compositor, &layers, output));
                progress.lock().unwrap().compose.add(t.elapsed());
                let failed = composed.is_err();
                if composed_tx.send(composed).is_err() || failed {
                    return;
                }
            }
        });

        // Inside the closure: if the encoder exits with an error `composed_rx` must
        // be closed before the join, or the upstream stages stay on `send`.
        let composed_rx = composed_rx;
        // The audio must be written along with the video: all of it after the video and
        // the muxer keeps the whole video in RAM and the interleaving becomes
        // quadratic (minutes on a timeline of a few minutes).
        let mut audio = AudioInterleaver::new(timeline, range.start);
        for frame in range.clone() {
            if cancel.load(Ordering::Relaxed) {
                return Err(ExportError::Cancelled);
            }
            let frame_i420 = match composed_rx.recv() {
                Ok(frame_i420) => frame_i420?,
                Err(_) => return Err(ExportError::Cancelled),
            };
            let t = std::time::Instant::now();
            encoder
                .write_video_frame(&frame_i420)
                .map_err(ExportError::failed)?;
            if audio_mix.as_ref().is_some_and(|h| h.is_finished()) {
                audio.mixed = Some(join_audio_mix(audio_mix.take())?);
            }
            audio.write_until(&mut encoder, frame + 1)?;

            let mut p = progress.lock().unwrap();
            p.encode.add(t.elapsed());
            p.current_frame = frame - range.start + 1;
            p.elapsed = started.elapsed();
            if p.startup.is_none() {
                p.startup = Some(p.elapsed);
            }
        }
        if audio_mix.is_some() {
            audio.mixed = Some(join_audio_mix(audio_mix)?);
        }
        audio.write_until(&mut encoder, range.end)
    })?;

    encoder.finish().map_err(ExportError::failed)?;
    finish();
    Ok(())
}

#[cfg(test)]
fn render_video_frame(
    project: &Project,
    timeline: &Timeline,
    compositor: &vv_render::Compositor,
    provider: &mut StreamingFrameProvider,
    frame: FrameIdx,
    resolution: (u32, u32),
) -> Result<Vec<u8>, ExportError> {
    let layers = decode_video_frame(project, timeline, provider, compositor, frame, resolution)?;
    let output = vv_render::OutputFrame::exact(resolution.0, resolution.1);
    Ok(compose_video_frame(compositor, &layers, output))
}

/// One timeline frame, decoded and composed as the export does it, in RGBA8
/// at `size` (the timeline's aspect ratio). Frame-accurate: it decodes the
/// frame itself, nothing comes from a cache.
pub fn render_frame_rgba(
    project: &Project,
    timeline_id: TimelineId,
    frame: FrameIdx,
    size: (u32, u32),
    compositor: &vv_render::Compositor,
) -> Result<Vec<u8>, ExportError> {
    let timeline = project
        .timelines
        .get(timeline_id)
        .ok_or(ExportError::TimelineNotFound)?;
    let mut provider = StreamingFrameProvider::default();
    let layers = decode_video_frame(
        project,
        timeline,
        &mut provider,
        compositor,
        frame,
        timeline.resolution,
    )?;
    let layers: Vec<vv_render::Layer> = layers.iter().map(OwnedLayer::as_render).collect();
    let output = vv_render::OutputFrame::scaled(size.0, size.1, timeline.resolution);
    Ok(compositor.render_layers(&layers, output))
}

/// The layers of the frame, from bottom to top. A missing media frame
/// (past the real end of the file) leaves out only that layer.
fn decode_video_frame(
    project: &Project,
    timeline: &Timeline,
    provider: &mut StreamingFrameProvider,
    compositor: &vv_render::Compositor,
    frame: FrameIdx,
    resolution: (u32, u32),
) -> Result<Vec<OwnedLayer>, ExportError> {
    let clips = timeline.active_video_clips_at(frame);
    // Every clip `track_layers_at` decodes, or `retain_clips` would close its
    // decoder and reopen it on the next frame.
    provider.retain_clips(&clips_decoded_at(project, timeline, frame));
    let mut gpu = GpuCompounds::new(provider, compositor);
    let mut layers = Vec::with_capacity(clips.len());
    for (track_index, clip) in clips {
        layers.extend(track_layers_at(
            project,
            timeline,
            track_index,
            clip,
            frame,
            resolution,
            &mut gpu,
        )?);
    }
    Ok(layers)
}

fn compose_video_frame(
    compositor: &vv_render::Compositor,
    layers: &[OwnedLayer],
    output: vv_render::OutputFrame,
) -> Vec<u8> {
    let layers: Vec<vv_render::Layer> = layers.iter().map(OwnedLayer::as_render).collect();
    compositor.render_layers_i420(&layers, output)
}

fn join_audio_mix(
    handle: Option<std::thread::ScopedJoinHandle<'_, Result<Vec<f32>, ExportError>>>,
) -> Result<Vec<f32>, ExportError> {
    handle
        .expect("audio mix already consumed")
        .join()
        .expect("audio mix thread panicked")
}

/// Writes the audio mix to the encoder in pieces, aligned to the video frames.
struct AudioInterleaver {
    mixed: Option<Vec<f32>>,
    written: usize,
    fps: f64,
    start_sample: u64,
}

impl AudioInterleaver {
    fn new(timeline: &Timeline, start_frame: FrameIdx) -> Self {
        let fps = timeline.fps.as_f64();
        Self {
            mixed: None,
            written: 0,
            fps,
            start_sample: timeline_frame_to_sample(start_frame, fps, PROJECT_SAMPLE_RATE),
        }
    }

    /// Writes the samples up to the start of `frame` (exclusive); no-op
    /// until the mix is ready.
    fn write_until(
        &mut self,
        encoder: &mut vv_media::Encoder,
        frame: FrameIdx,
    ) -> Result<(), ExportError> {
        let Some(mixed) = &self.mixed else {
            return Ok(());
        };
        let end_sample = timeline_frame_to_sample(frame, self.fps, PROJECT_SAMPLE_RATE);
        let end = ((end_sample - self.start_sample) as usize * PROJECT_CHANNELS as usize)
            .min(mixed.len());
        if end > self.written {
            encoder
                .write_audio_samples(&mixed[self.written..end])
                .map_err(ExportError::failed)?;
            self.written = end;
        }
        Ok(())
    }
}

/// Mix of all the audio tracks at `PROJECT_SAMPLE_RATE`/`PROJECT_CHANNELS`,
/// over the timeline frames in `range`: the same `mix_range` as the preview.
pub(crate) fn mix_audio_track(
    project: &Project,
    timeline: &Timeline,
    range: std::ops::Range<FrameIdx>,
) -> Result<Vec<f32>, ExportError> {
    let fps = timeline.fps.as_f64();
    let start_sample = timeline_frame_to_sample(range.start, fps, PROJECT_SAMPLE_RATE);
    let end_sample = timeline_frame_to_sample(range.end, fps, PROJECT_SAMPLE_RATE);
    let samples = start_sample..end_sample;
    let (rate, channels) = (PROJECT_SAMPLE_RATE, PROJECT_CHANNELS);
    let mut wanted = WantedFrames::default();
    MixSnapshot::from_timeline_range(
        project,
        timeline,
        rate,
        channels,
        &mut wanted,
        samples.clone(),
    );
    let mut audio = DecodedAudio::default();
    for (path, windows) in wanted.0 {
        decode_windows(&path, windows, &mut audio)?;
    }
    let snapshot =
        MixSnapshot::from_timeline_range(project, timeline, rate, channels, &mut audio, samples);
    let mut mixed = vec![0.0_f32; (end_sample - start_sample) as usize * PROJECT_CHANNELS as usize];
    mix_range(&snapshot, start_sample, &mut mixed);
    Ok(mixed)
}

/// Decodes the `(stream, frames)` windows of `path` in one pass, stopping
/// past the last one. The same decoding as the preview (`mix_buffers`),
/// from the start of the file: swresample to `PROJECT_SAMPLE_RATE`, so the
/// frames line up with the preview's.
fn decode_windows(
    path: &Path,
    windows: Vec<(usize, std::ops::Range<u64>)>,
    audio: &mut DecodedAudio,
) -> Result<(), ExportError> {
    let ch = PROJECT_CHANNELS as u64;
    let mut streams: Vec<usize> = windows.iter().map(|(stream, _)| *stream).collect();
    streams.sort_unstable();
    streams.dedup();
    let slot_of = |stream: usize| streams.iter().position(|s| *s == stream).unwrap();
    let mut ends = vec![0_u64; streams.len()];
    for (stream, frames) in &windows {
        let end = &mut ends[slot_of(*stream)];
        *end = (*end).max(frames.end);
    }
    let mut buffers = vec![Vec::new(); windows.len()];
    let mut decoded = vec![0_u64; streams.len()];
    let mut remixed = Vec::new();
    let formats = vv_media::decode_audio_streams_streaming(
        path,
        &streams,
        Some(PROJECT_SAMPLE_RATE),
        |slot, channels, chunk| {
            remixed.clear();
            remix_channels_into(chunk, channels, PROJECT_CHANNELS, &mut remixed);
            let from = decoded[slot];
            let to = from + remixed.len() as u64 / ch;
            decoded[slot] = to;
            for ((stream, frames), buffer) in windows.iter().zip(&mut buffers) {
                let (a, b) = (frames.start.max(from), frames.end.min(to));
                if *stream == streams[slot] && a < b {
                    buffer.extend_from_slice(
                        &remixed[((a - from) * ch) as usize..((b - from) * ch) as usize],
                    );
                }
            }
            if decoded
                .iter()
                .zip(&ends)
                .all(|(decoded, end)| decoded >= end)
            {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        },
    )
    .map_err(ExportError::failed)?;
    for ((stream, frames), buffer) in windows.into_iter().zip(buffers) {
        if formats[slot_of(stream)].is_some() {
            audio
                .files
                .insert((path.to_path_buf(), stream, frames), Arc::new(buffer));
        }
    }
    Ok(())
}

/// The frames of every real file the mix reaches, compound clips included,
/// without providing any.
#[derive(Default)]
struct WantedFrames(HashMap<PathBuf, Vec<(usize, std::ops::Range<u64>)>>);

impl AudioSource for WantedFrames {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        self.file_frames(path, stream, 0..u64::MAX).0
    }

    fn file_frames(
        &mut self,
        path: &Path,
        stream: usize,
        frames: std::ops::Range<u64>,
    ) -> (ClipAudio, u64) {
        let windows = self.0.entry(path.to_path_buf()).or_default();
        let window = (stream, frames);
        if !windows.contains(&window) {
            windows.push(window);
        }
        (ClipAudio::Pending, 0)
    }
}

/// Everything decoded up front: a compound used several times is mixed once.
#[derive(Default)]
struct DecodedAudio {
    files: HashMap<(PathBuf, usize, std::ops::Range<u64>), Arc<Vec<f32>>>,
    compounds: HashMap<MediaId, Arc<Vec<f32>>>,
}

impl AudioSource for DecodedAudio {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        self.file_frames(path, stream, 0..u64::MAX).0
    }

    fn file_frames(
        &mut self,
        path: &Path,
        stream: usize,
        frames: std::ops::Range<u64>,
    ) -> (ClipAudio, u64) {
        let start = frames.start;
        self.files
            .get(&(path.to_path_buf(), stream, frames))
            .map_or((ClipAudio::Missing, 0), |buffer| {
                (ClipAudio::Ready(buffer.clone()), start)
            })
    }

    fn cached_compound(&mut self, media_id: MediaId, _content_hash: u64) -> Option<Arc<Vec<f32>>> {
        self.compounds.get(&media_id).cloned()
    }

    fn store_compound(&mut self, media_id: MediaId, _content_hash: u64, mixdown: Arc<Vec<f32>>) {
        self.compounds.insert(media_id, mixdown);
    }
}

#[cfg(test)]
#[path = "tests/export.rs"]
mod tests;
