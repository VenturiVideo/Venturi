//! Reading the metadata of a media.

use ffmpeg_next as ffmpeg;
use std::path::Path;
use std::sync::Once;
use vv_core::{FrameIdx, MediaFileInfo, MediaMeta, Rational};

static INIT: Once = Once::new();

pub(crate) fn ensure_init() {
    INIT.call_once(|| {
        ffmpeg::init().expect("cannot initialize ffmpeg");
        // Warnings flood stderr: every MP3/Opus seek prints some.
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
    });
}

/// Nominal fps of an audio-only media: it has no frames of its own, but
/// `source_in`/`source_out` and the gain keyframes are counted in source
/// frames all the same.
pub const AUDIO_ONLY_FPS: Rational = Rational::new(30, 1);

/// Nominal fps of an image: only a consistent unit for `source_in`/`source_out`
/// is needed.
pub const IMAGE_FPS: Rational = Rational::new(25, 1);

/// Above this the declared fps is not a frame rate but the container's
/// tick rate: VFR recordings (Android screen capture, some phone cameras)
/// carry `r_frame_rate = 90000`, and trusting it would inflate
/// `duration_frames` by five orders of magnitude.
const MAX_PLAUSIBLE_FPS: f64 = 1000.0;

/// Fallback when neither declared rate is usable.
const UNKNOWN_FPS: Rational = Rational::new(30, 1);

/// Ceiling for the rate measured on a VFR stream: beyond this the cost of
/// conforming it to CFR outweighs the smoothness gained.
const MAX_MEASURED_FPS: i32 = 120;

/// `best(Video)` without the cover art: an audio file with an embedded
/// picture (MP3, WAV, FLAC...) has a one-frame video stream, and taking it
/// for the media's video cut the audio to that one frame.
pub(crate) fn video_stream(
    input: &ffmpeg::format::context::Input,
) -> Option<ffmpeg::format::stream::Stream<'_>> {
    let is_picture = |stream: &ffmpeg::format::stream::Stream| {
        stream.parameters().medium() != ffmpeg::media::Type::Video
            || stream
                .disposition()
                .contains(ffmpeg::format::stream::Disposition::ATTACHED_PIC)
    };
    input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .filter(|stream| !is_picture(stream))
        .or_else(|| input.streams().find(|stream| !is_picture(stream)))
}

/// The rate at which the media is decoded and indexed. `r_frame_rate` is
/// the finest tick the stream can express, not its real rate: phone screen
/// captures declare 90000 there, so it is only believed when plausible.
pub(crate) fn media_fps(path: &Path, video: &ffmpeg::format::stream::Stream) -> Rational {
    let rate = video.rate();
    if let Some(fps) = plausible_fps(rate.numerator(), rate.denominator()) {
        return fps;
    }
    // A VFR stream gets conformed to CFR, and the average rate would drop
    // the bursts: a clip averaging 4 fps because the screen is still can
    // still hold 60 fps of real motion. The peak is what makes those
    // stretches play smoothly.
    measured_peak_fps(path)
        .or_else(|| {
            let avg = video.avg_frame_rate();
            plausible_fps(avg.numerator(), avg.denominator())
        })
        .unwrap_or(UNKNOWN_FPS)
}

fn plausible_fps(num: i32, den: i32) -> Option<Rational> {
    (num > 0 && den > 0 && f64::from(num) / f64::from(den) <= MAX_PLAUSIBLE_FPS)
        .then(|| Rational::new(num, den))
}

/// Frames in the densest one-second window, demuxing the whole file without
/// decoding. Memoized: `Decoder::open` needs the same value as `probe` and
/// is called again on every seek far from the playhead.
fn measured_peak_fps(path: &Path) -> Option<Rational> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    type Key = (std::path::PathBuf, u64, i64);
    static CACHE: OnceLock<Mutex<HashMap<Key, Option<Rational>>>> = OnceLock::new();
    let meta = std::fs::metadata(path).ok()?;
    let key = (
        path.to_path_buf(),
        meta.len(),
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64),
    );
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().unwrap().get(&key) {
        return *hit;
    }
    let measured = peak_fps_of_window(&packet_times(path)?);
    cache.lock().unwrap().insert(key, measured);
    measured
}

fn packet_times(path: &Path) -> Option<Vec<f64>> {
    let mut input = ffmpeg::format::input(&path).ok()?;
    let stream = video_stream(&input)?;
    let stream_index = stream.index();
    let time_base = seconds_per_tick(stream.time_base());
    let mut times: Vec<f64> = input
        .packets()
        .filter(|(stream, _)| stream.index() == stream_index)
        .filter_map(|(_, packet)| packet.pts().map(|pts| pts as f64 * time_base))
        .collect();
    times.sort_by(f64::total_cmp);
    Some(times)
}

fn peak_fps_of_window(times: &[f64]) -> Option<Rational> {
    if times.len() < 2 {
        return None;
    }
    let mut start = 0usize;
    let mut peak = 0usize;
    for end in 0..times.len() {
        while times[end] - times[start] >= 1.0 {
            start += 1;
        }
        peak = peak.max(end - start + 1);
    }
    if peak < 2 {
        return None;
    }
    Some(Rational::new(
        snapped_to_a_common_rate(peak as i32).min(MAX_MEASURED_FPS),
        1,
    ))
}

/// The count in the densest window is off by a frame or two — pts jitter at
/// one end of the window, a dropped frame at the other. A capture device
/// targets a standard rate, so the nearby one is the real answer.
fn snapped_to_a_common_rate(measured: i32) -> i32 {
    const COMMON: [i32; 8] = [24, 25, 30, 48, 50, 60, 90, 120];
    COMMON
        .into_iter()
        .filter(|&rate| (measured - rate).abs() * 10 <= rate)
        // On a tie the higher rate wins: conforming too low drops real
        // frames, too high only repeats them.
        .min_by_key(|&rate| ((measured - rate).abs(), -rate))
        .unwrap_or(measured)
}

pub fn probe(path: &Path) -> Result<MediaMeta, crate::MediaError> {
    ensure_init();
    let input = ffmpeg::format::input(&path)?;

    let video = match video_stream(&input) {
        Some(video) => {
            let fps = media_fps(path, &video);
            let decoder = ffmpeg::codec::context::Context::from_parameters(video.parameters())?
                .decoder()
                .video()?;
            let (width, height) = crate::orientation::Orientation::from_stream(&video)
                .dimensions(decoder.width(), decoder.height());
            Some((fps, width, height))
        }
        None => None,
    };

    let audio = input.streams().best(ffmpeg::media::Type::Audio);
    let (has_audio, sample_rate, channels) = match audio {
        Some(audio) => {
            let audio_decoder =
                ffmpeg::codec::context::Context::from_parameters(audio.parameters())?
                    .decoder()
                    .audio()?;
            (true, audio_decoder.rate(), audio_decoder.channels())
        }
        None => (false, 0, 0),
    };
    if video.is_none() && !has_audio {
        return Err(crate::MediaError::NoStream(path.display().to_string()));
    }

    let audio_streams = input
        .streams()
        .filter(|s| s.parameters().medium() == ffmpeg::media::Type::Audio)
        .count() as u16;
    let (fps, width, height) = video.unwrap_or((AUDIO_ONLY_FPS, 0, 0));
    let duration_secs = input.duration() as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE);
    let nominal_duration_frames = (duration_secs * fps.as_f64()).round() as i64;
    // The container often declares more frames than the decoder actually
    // produces (drop frames, imprecise CFR index...): whoever asks for
    // frame `duration_frames - 1` as the "last frame" (e.g. the freeze in
    // `held_timeline_frame`) would get `None` forever if we blindly
    // trusted the arithmetic.
    let duration_frames = if video.is_some() {
        verify_last_decodable_frame(path, fps, nominal_duration_frames)
    } else {
        nominal_duration_frames
    };
    Ok(MediaMeta {
        duration_frames,
        fps,
        width,
        height,
        has_video: video.is_some(),
        has_audio,
        sample_rate,
        channels,
        audio_streams,
        file: file_info(path, &input),
    })
}

/// Corrects `nominal` (duration*fps) to the true count of frames the stream
/// produces. Two passes, both on the tail of the file only: a demux (no
/// decoding) that finds the pts of the last video packet and of the last
/// keyframe, then decoding of the last GOP alone to confirm that that frame
/// really comes out of the decoder. It never raises `nominal`: if something
/// fails the arithmetic value is kept and the excess, if any, is ignored —
/// this fix corrects only the overestimate, it does not introduce a new
/// one.
fn verify_last_decodable_frame(path: &Path, fps: Rational, nominal: i64) -> i64 {
    let idx_of = |secs: f64| (secs * fps.as_f64()).round() as FrameIdx;
    let Some((last_packet_secs, last_key_secs)) = scan_tail(path, fps, nominal) else {
        return nominal;
    };
    match decode_from(path, last_key_secs) {
        Some(secs) => (idx_of(secs) + 1).min(nominal),
        // The final GOP does not decode: stop at the demux, which is
        // closer to the truth than the arithmetic anyway.
        None => (idx_of(last_packet_secs) + 1).min(nominal),
    }
}

/// `(pts of the last video packet, pts of the last keyframe)` in seconds,
/// demuxing from the tail of the stream. If the seek lands past the real
/// end (which is the case we are fixing) it re-reads from the start:
/// without decoding it costs a few ms even on long files.
fn scan_tail(path: &Path, fps: Rational, nominal: i64) -> Option<(f64, f64)> {
    const SAFETY_MARGIN_SECS: f64 = 15.0;
    let nominal_secs = nominal as f64 / fps.as_f64();
    for seek_secs in [(nominal_secs - SAFETY_MARGIN_SECS).max(0.0), 0.0] {
        let mut input = ffmpeg::format::input(&path).ok()?;
        let stream = video_stream(&input)?;
        let stream_index = stream.index();
        let time_base = seconds_per_tick(stream.time_base());
        let ts = (seek_secs * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64;
        if input.seek(ts, ..ts).is_err() {
            continue;
        }
        let mut last = None;
        let mut last_key = 0.0f64;
        for (stream, packet) in input.packets() {
            if stream.index() != stream_index {
                continue;
            }
            let Some(secs) = packet.pts().map(|pts| pts as f64 * time_base) else {
                continue;
            };
            last = Some(last.map_or(secs, |max: f64| max.max(secs)));
            if packet.is_key() {
                last_key = last_key.max(secs);
            }
        }
        if let Some(last) = last {
            return Some((last, last_key));
        }
        if seek_secs == 0.0 {
            break;
        }
    }
    None
}

/// Pts of the last frame the decoder produces starting from the keyframe at
/// `from_secs`, in seconds. No scaler: only the timestamp is needed, not
/// the pixels.
fn decode_from(path: &Path, from_secs: f64) -> Option<f64> {
    let mut input = ffmpeg::format::input(&path).ok()?;
    let stream = video_stream(&input)?;
    let stream_index = stream.index();
    let time_base = seconds_per_tick(stream.time_base());
    let mut context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .ok()?
        .decoder();
    context.set_threading(ffmpeg::threading::Config {
        kind: ffmpeg::threading::Type::Frame,
        count: 0,
    });
    let mut decoder = context.video().ok()?;
    let ts = (from_secs * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64;
    input.seek(ts, ..ts).ok()?;

    let mut frame = ffmpeg::frame::Video::empty();
    let mut last: Option<f64> = None;
    let mut drain = |decoder: &mut ffmpeg::decoder::Video, last: &mut Option<f64>| {
        while decoder.receive_frame(&mut frame).is_ok() {
            let Some(secs) = frame.pts().map(|pts| pts as f64 * time_base) else {
                continue;
            };
            *last = Some(last.map_or(secs, |max: f64| max.max(secs)));
        }
    };
    for (stream, packet) in input.packets() {
        if stream.index() != stream_index {
            continue;
        }
        if decoder.send_packet(&packet).is_ok() {
            drain(&mut decoder, &mut last);
        }
    }
    if decoder.send_eof().is_ok() {
        drain(&mut decoder, &mut last);
    }
    last
}

fn seconds_per_tick(time_base: ffmpeg::Rational) -> f64 {
    time_base.numerator() as f64 / time_base.denominator() as f64
}

/// Extensions treated as images: probing a container is not enough to
/// tell them apart.
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "bmp", "webp", "tif", "tiff"];

pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|img| img.eq_ignore_ascii_case(ext))
        })
}

pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "avi", "webm", "m4v"];
pub const AUDIO_EXTENSIONS: &[&str] = &["wav", "mp3", "flac", "m4a", "aac", "ogg", "opus"];

/// Whether the extension is one of the media Venturi imports.
pub fn is_media_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    [VIDEO_EXTENSIONS, AUDIO_EXTENSIONS, IMAGE_EXTENSIONS]
        .concat()
        .iter()
        .any(|known| known.eq_ignore_ascii_case(ext))
}

/// `probe_image` or `probe`, by extension.
pub fn probe_media(path: &Path) -> Result<MediaMeta, crate::MediaError> {
    if is_image_path(path) {
        probe_image(path)
    } else {
        probe(path)
    }
}

/// Like `probe` but for an image: `duration_frames` is the sentinel
/// `vv_core::IMAGE_DURATION_FRAMES`.
pub fn probe_image(path: &Path) -> Result<MediaMeta, crate::MediaError> {
    ensure_init();
    let input = ffmpeg::format::input(&path)?;
    let video = input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or_else(|| crate::MediaError::NoStream(path.display().to_string()))?;
    let decoder = ffmpeg::codec::context::Context::from_parameters(video.parameters())?
        .decoder()
        .video()?;
    Ok(MediaMeta {
        duration_frames: vv_core::IMAGE_DURATION_FRAMES,
        fps: IMAGE_FPS,
        width: decoder.width(),
        height: decoder.height(),
        has_video: true,
        has_audio: false,
        sample_rate: 0,
        channels: 0,
        audio_streams: 0,
        file: file_info(path, &input),
    })
}

fn file_info(path: &Path, input: &ffmpeg::format::context::Input) -> MediaFileInfo {
    let codec_name = |s: &ffmpeg::format::stream::Stream| s.parameters().id().name().to_owned();
    let audio = input.streams().best(ffmpeg::media::Type::Audio);
    let tag = |keys: &[&str]| {
        let find = |dict: ffmpeg::DictionaryRef| {
            dict.iter()
                .find(|(k, v)| {
                    keys.iter().any(|key| k.eq_ignore_ascii_case(key)) && !v.trim().is_empty()
                })
                .map(|(_, v)| v.trim().to_owned())
        };
        // Ogg/Opus keep their tags on the stream, not on the container.
        find(input.metadata()).or_else(|| audio.as_ref().and_then(|s| find(s.metadata())))
    };
    MediaFileInfo {
        size_bytes: std::fs::metadata(path).ok().map(|m| m.len()),
        video_codec: video_stream(input).as_ref().map(codec_name),
        audio_codec: audio.as_ref().map(codec_name),
        title: tag(&["title"]),
        artist: tag(&["artist", "author", "album_artist"]),
    }
}

/// `MediaMeta::file` alone, without the (slow) frame count verification.
pub fn probe_file_info(path: &Path) -> Result<MediaFileInfo, crate::MediaError> {
    ensure_init();
    let input = ffmpeg::format::input(&path)?;
    Ok(file_info(path, &input))
}

/// Every metadata tag of the container and of its streams, as ffprobe lists
/// them.
pub fn probe_tags(path: &Path) -> Result<Vec<(String, String)>, crate::MediaError> {
    ensure_init();
    let input = ffmpeg::format::input(&path)?;
    let mut tags: Vec<(String, String)> = input
        .metadata()
        .iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    for stream in input.streams() {
        tags.extend(
            stream
                .metadata()
                .iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned())),
        );
    }
    Ok(tags)
}

/// Info on an audio stream of the container, for multi-track import
/// (see `audio_streams`).
pub struct AudioStreamInfo {
    pub sample_rate: u32,
    pub channels: u16,
}

/// All the audio streams in container order (not ffmpeg's "best"): this is
/// the index of `Clip::audio_stream_index`. A multi-audio file is imported
/// with one clip per stream.
pub fn audio_streams(path: &Path) -> Result<Vec<AudioStreamInfo>, crate::MediaError> {
    ensure_init();
    let input = ffmpeg::format::input(&path)?;
    let mut out = Vec::new();
    for stream in input.streams() {
        if stream.parameters().medium() != ffmpeg::media::Type::Audio {
            continue;
        }
        let decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?
            .decoder()
            .audio()?;
        out.push(AudioStreamInfo {
            sample_rate: decoder.rate(),
            channels: decoder.channels(),
        });
    }
    Ok(out)
}

/// Fingerprint (canonical path + size + mtime, FNV-1a) used as the
/// `content_hash` for the derived caches. It does not read the bytes: a file
/// overwritten with the same size and mtime would use a stale cache, a
/// tradeoff accepted for instant imports. Stable across restarts, unlike
/// `DefaultHasher`.
pub fn content_fingerprint(path: &Path) -> std::io::Result<u64> {
    let metadata = std::fs::metadata(path)?;
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mtime_secs = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3); // FNV-1a prime
        }
    };
    feed(canonical.to_string_lossy().as_bytes());
    feed(&metadata.len().to_le_bytes());
    feed(&mtime_secs.to_le_bytes());
    Ok(hash)
}

#[cfg(test)]
#[path = "tests/probe.rs"]
mod tests;
