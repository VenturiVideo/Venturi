use super::*;
use vv_audio::mixer::ClipInput;
use vv_audio::mixer::{mix_range, timeline_frame_to_sample};
use vv_core::{
    AudioEffect, AudioEffectKind, Clip, ClipId, ClipSource, Equalizer, MediaId, MediaItem,
    MediaMeta, MultibandCompressor, NormalizeMode, Rational, SetLevel, Track, TrackKind,
};

const FPS: u64 = 25;
const FRAME_SAMPLES: u64 = PROJECT_SAMPLE_RATE as u64 / FPS;

fn source(name: &str, args: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join("vv-audio-render-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    vv_media::test_support::ffmpeg(args, &path);
    path
}

fn chirp(name: &str, rate: u32, secs: u32) -> PathBuf {
    let lavfi = format!("aevalsrc=0.8*sin(300*t*t):sample_rate={rate}:duration={secs}");
    source(name, &["-f", "lavfi", "-i", &lavfi])
}

fn media(project: &mut Project, path: PathBuf, sample_rate: u32, audio_streams: u16) -> MediaId {
    project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 10_000,
            fps: Rational::new(FPS as i32, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate,
            channels: 1,
            audio_streams,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: None,
    })
}

fn clip(id: u64, media: MediaId, source_in: u64, len: u64, start: u64) -> Clip {
    Clip::from_source_range(
        ClipId(id),
        ClipSource::Media(media),
        source_in as i64,
        (source_in + len) as i64,
        start as i64,
        Rational::one(),
    )
}

fn timeline(tracks: Vec<Track>) -> Timeline {
    Timeline {
        name: "t".into(),
        fps: Rational::new(FPS as i32, 1),
        resolution: (4, 2),
        tracks,
        markers: Vec::new(),
        master: Default::default(),
    }
}

fn audio_track(clips: Vec<Clip>) -> Track {
    Track {
        clips,
        ..Track::new(TrackKind::Audio)
    }
}

#[test]
fn clip_pitch_shift_preview_and_export_agree_without_changing_placement_or_duration() {
    let mut project = Project::default();
    let path = source(
        "pitch_shift.wav",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=4",
        ],
    );
    let file = media(&mut project, path, 48_000, 1);
    let cases = [
        (Rational::one(), true, 12.0, true, false, 880.0),
        (Rational::one(), true, -12.0, true, true, 220.0),
        (Rational::one(), true, 12.0, false, false, 440.0),
        (Rational::new(2, 1), true, 12.0, true, false, 880.0),
        (Rational::new(2, 1), false, -12.0, true, false, 440.0),
        (Rational::new(1, 2), false, 12.0, true, false, 440.0),
    ];
    for (speed, correction, semitones, enabled, preserve_formants, expected) in cases {
        let mut shifted = Clip::new(
            ClipId(1),
            ClipSource::Media(file),
            10,
            10,
            25,
            Rational::one(),
            speed,
        );
        shifted.pitch_correction = correction;
        shifted.effects.pitch_shift = vv_core::PitchShift {
            enabled,
            semitones,
            preserve_formants,
            ..Default::default()
        };
        let tl = timeline(vec![audio_track(vec![shifted])]);
        let range = samples(0..45);
        let preview = mixed_whole(&project, &tl, range.clone());
        let export = rendered(&project, &tl, range);
        assert_eq!(
            preview, export,
            "speed {speed:?}, correction {correction}, pitch {semitones}"
        );
        assert_eq!(
            export.len(),
            samples(0..45).end as usize * PROJECT_CHANNELS as usize
        );
        assert!(
            export[..samples(0..10).end as usize * PROJECT_CHANNELS as usize]
                .iter()
                .all(|s| *s == 0.0)
        );
        assert!(
            export[samples(0..35).end as usize * PROJECT_CHANNELS as usize..]
                .iter()
                .all(|s| *s == 0.0)
        );
        let window = &export[samples(0..16).end as usize * PROJECT_CHANNELS as usize
            ..samples(0..29).end as usize * PROJECT_CHANNELS as usize];
        let mono: Vec<f32> = window
            .iter()
            .step_by(PROJECT_CHANNELS as usize)
            .copied()
            .collect();
        let crossings = mono
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count();
        let heard = crossings as f64 * PROJECT_SAMPLE_RATE as f64 / mono.len() as f64;
        assert!(
            (heard - expected).abs() < 8.0,
            "expected {expected}, heard {heard}"
        );
        let seek = rendered(&project, &tl, samples(13..30));
        assert_eq!(
            seek,
            export[samples(0..13).end as usize * PROJECT_CHANNELS as usize
                ..samples(0..30).end as usize * PROJECT_CHANNELS as usize]
        );
    }
}

#[test]
fn pitch_shifted_clips_with_different_keep_pitch_export_independently() {
    let mut project = Project::default();
    let path = source(
        "keep_pitch_cache.wav",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=4",
        ],
    );
    let file = media(&mut project, path, 48_000, 1);
    let mut left = Clip::new(
        ClipId(1),
        ClipSource::Media(file),
        0,
        0,
        25,
        Rational::one(),
        Rational::new(2, 1),
    );
    left.pitch_correction = true;
    left.effects.pitch_shift.semitones = 12.0;
    let mut right = left.clone();
    right.id = ClipId(2);
    right.timeline_start = 25;
    right.pitch_correction = false;
    let isolated = timeline(vec![audio_track(vec![right.clone()])]);
    let together = timeline(vec![audio_track(vec![left, right])]);
    let reference = rendered(&project, &isolated, samples(25..50));
    let full = rendered(&project, &together, samples(0..50));
    let actual = &full[samples(0..25).end as usize * PROJECT_CHANNELS as usize..];
    let second_half = 24_000 * PROJECT_CHANNELS as usize;
    let reference_peak = reference[second_half..]
        .iter()
        .fold(0.0_f32, |m, v| m.max(v.abs()));
    let actual_peak = actual[second_half..]
        .iter()
        .fold(0.0_f32, |m, v| m.max(v.abs()));
    assert!(reference_peak > 0.01);
    assert!(
        actual_peak > 0.01,
        "the second clip becomes silent halfway through when the other clip populated the cache"
    );
    assert_eq!(actual, reference);
}

#[test]
fn pitch_shifting_an_aac_tone_preserves_its_continuous_phase() {
    let mut project = Project::default();
    let path = source(
        "pitch_continuity.m4a",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=3",
        ],
    );
    let file = media(&mut project, path, 48_000, 1);
    let mut shifted = clip(1, file, 0, 75, 0);
    shifted.effects.pitch_shift.semitones = 12.0;
    let tl = timeline(vec![audio_track(vec![shifted])]);
    let output = rendered(&project, &tl, samples(0..75));
    let mono: Vec<f64> = output
        .iter()
        .step_by(PROJECT_CHANNELS as usize)
        .map(|x| f64::from(*x))
        .collect();
    let window_size = PROJECT_SAMPLE_RATE as usize / 10;
    let omega = std::f64::consts::TAU * 880.0 / f64::from(PROJECT_SAMPLE_RATE);
    let mut previous_phase: Option<f64> = None;
    for window in mono[window_size * 2..mono.len() - window_size * 2].chunks_exact(window_size) {
        let n = window.len() as f64;
        let a = 2.0
            * window
                .iter()
                .enumerate()
                .map(|(i, x)| x * (omega * i as f64).cos())
                .sum::<f64>()
            / n;
        let b = 2.0
            * window
                .iter()
                .enumerate()
                .map(|(i, x)| x * (omega * i as f64).sin())
                .sum::<f64>()
            / n;
        let power = window.iter().map(|x| x * x).sum::<f64>() / n;
        assert!(power > 0.001, "the shifted tone must stay audible");
        let residual = ((power - (a * a + b * b) * 0.5).max(0.0) / power).sqrt();
        assert!(
            residual < 0.08,
            "continuous tone distorted: residual {residual}"
        );
        let phase = b.atan2(a);
        if let Some(previous) = previous_phase {
            let jump = (phase - previous)
                .sin()
                .atan2((phase - previous).cos())
                .abs();
            assert!(jump < 0.15, "unexpected phase jump of {jump} radians");
        }
        previous_phase = Some(phase);
    }
}

#[test]
fn a_pitch_shifted_compound_clip_matches_preview_and_export() {
    let mut project = Project::default();
    let file = media(
        &mut project,
        chirp("pitch_compound.wav", 48_000, 3),
        48_000,
        1,
    );
    let mut inner = clip(1, file, 0, 50, 0);
    inner.effects.pitch_shift.semitones = -7.0;
    let nested = project
        .timelines
        .insert(timeline(vec![audio_track(vec![inner])]));
    let compound = project.media_pool.insert(MediaItem {
        compound: Some(nested),
        ..project.media_pool[file].clone()
    });
    let mut outer = clip(2, compound, 0, 50, 0);
    outer.effects.pitch_shift.semitones = 12.0;
    let tl = timeline(vec![audio_track(vec![outer])]);
    let range = samples(0..50);
    let preview = mixed_whole(&project, &tl, range.clone());
    assert!(preview.iter().any(|s| s.abs() > 0.01));
    assert_eq!(rendered(&project, &tl, range), preview);
}

fn samples(frames: std::ops::Range<u64>) -> Range<u64> {
    frames.start * FRAME_SAMPLES..frames.end * FRAME_SAMPLES
}

fn decoded_whole(path: &Path, stream: usize) -> Option<SampleBlocks> {
    let mut samples = Vec::new();
    let formats = vv_media::decode_audio_streams_streaming(
        path,
        &[stream],
        Some(PROJECT_SAMPLE_RATE),
        |_, channels, chunk| {
            remix_channels_into(chunk, channels, PROJECT_CHANNELS, &mut samples);
            ControlFlow::Continue(())
        },
    )
    .ok()?;
    formats[0]?;
    Some(SampleBlocks::contiguous(
        Arc::new(samples),
        0,
        PROJECT_CHANNELS,
    ))
}

/// The mix of `range` in one call, every file decoded whole from its start
/// and every level measured whole from them.
fn mixed_whole(project: &Project, tl: &Timeline, range: Range<u64>) -> Vec<f32> {
    let snapshot = MixSnapshot::from_timeline(
        project,
        tl,
        PROJECT_SAMPLE_RATE,
        PROJECT_CHANNELS,
        &mut decoded_whole,
    );
    let mut out = vec![0.0; (range.end - range.start) as usize * PROJECT_CHANNELS as usize];
    mix_range(&snapshot, range.start, &mut out);
    out
}

fn rendered(project: &Project, tl: &Timeline, range: Range<u64>) -> Vec<f32> {
    let mut out = Vec::new();
    render_offline(project, tl, range, &mut LevelCache::default(), |chunk| {
        out.extend_from_slice(chunk);
        ControlFlow::Continue(())
    })
    .unwrap();
    out
}

fn effect(kind: AudioEffectKind) -> AudioEffect {
    AudioEffect::new(kind)
}

fn boosting_eq() -> Equalizer {
    let mut eq = Equalizer::DEFAULT;
    eq.bands[2].gain_db = 6.0;
    eq
}

/// Several chunks, through a compressor and an equalizer whose state must
/// carry from one chunk to the next, over a seekable file used twice, a
/// resampled one at varispeed and a pitch-corrected clip.
#[test]
fn a_render_in_chunks_equals_one_mix_of_the_whole_timeline() {
    let mut project = Project::default();
    let seekable = media(&mut project, chirp("seekable.wav", 48_000, 20), 48_000, 1);
    let resampled = media(&mut project, chirp("resampled.wav", 44_100, 20), 44_100, 1);
    let mut first = audio_track(vec![
        clip(1, seekable, 10, 300, 0),
        clip(2, seekable, 50, 120, 320),
    ]);
    first.mix.effects = vec![
        effect(AudioEffectKind::MultibandCompressor(
            MultibandCompressor::DEFAULT,
        )),
        effect(AudioEffectKind::Equalizer(boosting_eq())),
    ];
    let varispeed = Clip::new(
        ClipId(3),
        ClipSource::Media(resampled),
        0,
        40,
        200,
        Rational::one(),
        Rational::new(3, 2),
    );
    let mut stretched = clip(4, resampled, 100, 100, 260);
    stretched.set_speed(Rational::new(5, 4), Rational::one());
    stretched.pitch_correction = true;
    let tl = timeline(vec![first, audio_track(vec![varispeed, stretched])]);

    let range = samples(0..480);
    assert!(range.end - range.start > 3 * CHUNK_FRAMES);
    let whole = mixed_whole(&project, &tl, range.clone());
    let chunked = rendered(&project, &tl, range);
    assert_eq!(chunked.len(), whole.len());
    let first_diff = chunked.iter().zip(&whole).position(|(a, b)| a != b);
    assert_eq!(first_diff, None, "the chunked render differs");
    assert!(chunked.iter().any(|s| s.abs() > 0.1));
}

#[test]
fn a_range_reads_only_the_windows_near_it() {
    let mut project = Project::default();
    let file = media(&mut project, chirp("near.wav", 48_000, 20), 48_000, 1);
    let tl = timeline(vec![audio_track(vec![clip(1, file, 0, 500, 0)])]);
    let mut wanted = Wanted {
        windows: HashSet::new(),
    };
    let range = samples(100..110);
    MixSnapshot::from_timeline_range(
        &project,
        &tl,
        PROJECT_SAMPLE_RATE,
        PROJECT_CHANNELS,
        &mut wanted,
        range.clone(),
    );
    assert_eq!(wanted.windows.len(), 1);
    let (_, _, frames) = wanted.windows.iter().next().unwrap();
    assert!(
        frames.start >= range.start && frames.end < range.end + PROJECT_SAMPLE_RATE as u64,
        "{frames:?}"
    );
}

/// The point of the render: what it holds does not grow with the timeline.
#[test]
fn the_render_holds_only_the_audio_near_where_it_is() {
    let mut project = Project::default();
    let file = media(&mut project, chirp("long.wav", 48_000, 60), 48_000, 1);
    let tl = timeline(vec![audio_track(vec![clip(1, file, 0, 1500, 0)])]);
    let mut cache = LevelCache::default();
    let mut audio = StreamingAudio::new(&mut cache);
    let end = samples(0..1500).end;
    let bound = (2 * CHUNK_FRAMES + READ_AHEAD_FRAMES) as usize * PROJECT_CHANNELS as usize;
    let mut at = 0;
    while at < end {
        let chunk = at..(at + CHUNK_FRAMES).min(end);
        audio
            .snapshot(&project, Mix::Timeline(&tl), chunk.clone(), false)
            .unwrap();
        // The windows share the readers' blocks.
        let held: usize = audio
            .readers
            .iter()
            .flat_map(|r| &r.kept)
            .map(|kept| kept.blocks.iter().map(|b| b.len()).sum::<usize>() + kept.filling.len())
            .sum();
        assert!(held <= bound, "{held} samples held at {chunk:?}");
        assert!(audio.readers.len() <= 1);
        at = chunk.end;
    }
}

#[test]
fn the_streams_of_a_file_read_at_the_same_place_share_one_decode() {
    let path = source(
        "two_streams.mkv",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=12",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=660:sample_rate=48000:duration=12",
            "-map",
            "0",
            "-map",
            "1",
            "-c:a",
            "pcm_s16le",
        ],
    );
    let mut project = Project::default();
    let file = media(&mut project, path, 48_000, 2);
    let mut second = clip(2, file, 0, 300, 0);
    second.audio_stream_index = 1;
    let tl = timeline(vec![
        audio_track(vec![clip(1, file, 0, 300, 0)]),
        audio_track(vec![second]),
    ]);
    let mut cache = LevelCache::default();
    let mut audio = StreamingAudio::new(&mut cache);
    audio
        .snapshot(&project, Mix::Timeline(&tl), 0..CHUNK_FRAMES, false)
        .unwrap();
    assert_eq!(audio.readers.len(), 1);
    assert_eq!(audio.readers[0].streams, [0, 1]);

    let range = samples(0..300);
    assert_eq!(
        rendered(&project, &tl, range.clone()),
        mixed_whole(&project, &tl, range)
    );
}

#[test]
fn a_stream_the_file_does_not_have_is_silent() {
    let mut project = Project::default();
    let file = media(&mut project, chirp("one_stream.wav", 48_000, 2), 48_000, 1);
    let mut missing = clip(1, file, 0, 50, 0);
    missing.audio_stream_index = 3;
    let tl = timeline(vec![audio_track(vec![missing])]);
    let mixed = rendered(&project, &tl, samples(0..50));
    assert!(mixed.iter().all(|s| *s == 0.0));
}

#[test]
fn a_break_from_the_sink_stops_the_render() {
    let mut project = Project::default();
    let file = media(&mut project, chirp("stop.wav", 48_000, 20), 48_000, 1);
    let tl = timeline(vec![audio_track(vec![clip(1, file, 0, 500, 0)])]);
    let mut chunks = 0;
    render_offline(
        &project,
        &tl,
        samples(0..500),
        &mut LevelCache::default(),
        |_| {
            chunks += 1;
            ControlFlow::Break(())
        },
    )
    .unwrap();
    assert_eq!(chunks, 1);
}

#[test]
fn a_range_starting_inside_the_timeline_is_that_part_of_the_mix() {
    let mut project = Project::default();
    let file = media(&mut project, chirp("part.wav", 48_000, 20), 48_000, 1);
    let mut track = audio_track(vec![clip(1, file, 0, 500, 0)]);
    track.mix.effects = vec![effect(AudioEffectKind::Equalizer(boosting_eq()))];
    let tl = timeline(vec![track]);
    let start = timeline_frame_to_sample(237, FPS as f64, PROJECT_SAMPLE_RATE);
    let range = start..samples(0..480).end;
    assert_eq!(
        rendered(&project, &tl, range.clone()),
        mixed_whole(&project, &tl, range)
    );
}

/// Each chunk is a new snapshot: the compound's own compressor carries on
/// across them like the outer ones.
#[test]
fn a_compound_rendered_in_chunks_equals_one_mix() {
    let mut project = Project::default();
    let file = media(&mut project, chirp("nested.wav", 48_000, 20), 48_000, 1);
    let mut nested_track = audio_track(vec![clip(1, file, 0, 400, 0)]);
    nested_track.mix.effects = vec![effect(AudioEffectKind::MultibandCompressor(
        MultibandCompressor::DEFAULT,
    ))];
    let nested = project.timelines.insert(timeline(vec![nested_track]));
    let compound = project.media_pool.insert(MediaItem {
        compound: Some(nested),
        ..project.media_pool[file].clone()
    });
    let mut varispeed = Clip::new(
        ClipId(2),
        ClipSource::Media(compound),
        0,
        200,
        150,
        Rational::one(),
        Rational::new(3, 2),
    );
    varispeed.pitch_correction = false;
    let tl = timeline(vec![
        audio_track(vec![clip(1, compound, 0, 300, 10)]),
        audio_track(vec![varispeed]),
    ]);
    let range = samples(0..400);
    let whole = mixed_whole(&project, &tl, range.clone());
    assert_eq!(rendered(&project, &tl, range), whole);
    assert!(whole.iter().any(|s| s.abs() > 0.1));
}

/// Every level asked for, measured by segment through `cache` and whole
/// from the decoded files.
struct BothMeasures<'c> {
    cache: &'c mut LevelCache,
    /// By segments, whole.
    levels: Vec<(f32, f32)>,
}

impl AudioSource for BothMeasures<'_> {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        decoded_whole(path, stream).map_or(ClipAudio::Missing, ClipAudio::Ready)
    }

    fn level(&mut self, analysis: LevelAnalysis) -> LevelReading {
        let by_segment = measure_level(&analysis, self.cache).unwrap();
        let whole = analysis.measure_with(&mut decoded_whole);
        self.levels.push((by_segment, whole));
        LevelReading::Ready(whole)
    }
}

fn both_measures(project: &Project, tl: &Timeline, cache: &mut LevelCache) -> Vec<(f32, f32)> {
    let mut source = BothMeasures {
        cache,
        levels: Vec::new(),
    };
    MixSnapshot::from_timeline(
        project,
        tl,
        PROJECT_SAMPLE_RATE,
        PROJECT_CHANNELS,
        &mut source,
    );
    source.levels
}

fn normalize(mode: NormalizeMode) -> AudioEffect {
    effect(AudioEffectKind::Normalize {
        target_db: -1.0,
        mode,
        set_level: SetLevel::Relative,
    })
}

/// Two tracks cut in several places, measured on the master.
fn edited_timeline(project: &mut Project, name: &str) -> Timeline {
    let file = media(project, chirp(name, 48_000, 20), 48_000, 1);
    let quiet = media(
        project,
        chirp(&format!("quiet-{name}"), 44_100, 20),
        44_100,
        1,
    );
    let mut voice = audio_track(vec![
        clip(1, file, 0, 60, 0),
        clip(2, file, 100, 45, 70),
        clip(3, file, 200, 80, 130),
    ]);
    voice.mix.gain_db = -3.0;
    let mut music = audio_track(vec![clip(4, quiet, 0, 150, 40)]);
    music.mix.gain_db = -18.0;
    timeline(vec![voice, music])
}

fn db(a: f32, b: f32) -> f32 {
    20.0 * (a / b).log10()
}

#[test]
fn a_level_measured_by_segment_reads_as_the_whole_measure() {
    let mut project = Project::default();
    let mut tl = edited_timeline(&mut project, "segments.wav");
    for mode in NormalizeMode::ALL {
        tl.master.effects = vec![normalize(mode)];
        let levels = both_measures(&project, &tl, &mut LevelCache::default());
        let [(by_segment, whole)] = levels[..] else {
            panic!("{levels:?}");
        };
        assert!(whole > 0.01);
        let tolerance = if mode == NormalizeMode::SamplePeak {
            1e-6
        } else {
            0.1
        };
        assert!(
            db(by_segment, whole).abs() < tolerance,
            "{mode:?}: {by_segment} vs {whole}"
        );
    }
}

#[test]
fn a_level_after_a_compressor_reads_as_the_whole_measure() {
    let mut project = Project::default();
    let mut tl = edited_timeline(&mut project, "compressed.wav");
    tl.tracks[0].mix.effects = vec![effect(AudioEffectKind::MultibandCompressor(
        MultibandCompressor::DEFAULT,
    ))];
    tl.master.effects = vec![
        effect(AudioEffectKind::Equalizer(boosting_eq())),
        normalize(NormalizeMode::Loudness),
    ];
    let levels = both_measures(&project, &tl, &mut LevelCache::default());
    let [(by_segment, whole)] = levels[..] else {
        panic!("{levels:?}");
    };
    assert!(db(by_segment, whole).abs() < 0.1, "{by_segment} vs {whole}");
}

/// The point of the segments: a ripple measures nothing again, an edit
/// only what it touches.
#[test]
fn an_edit_measures_again_only_the_segments_it_touches() {
    let mut project = Project::default();
    let mut tl = edited_timeline(&mut project, "edits.wav");
    tl.tracks.truncate(1);
    tl.master.effects = vec![normalize(NormalizeMode::Loudness)];
    let mut cache = LevelCache::default();
    both_measures(&project, &tl, &mut cache);
    assert_eq!(cache.rendered, 3, "one segment per clip");

    both_measures(&project, &tl, &mut cache);
    assert_eq!(cache.rendered, 3, "measured already");

    for clip in &mut tl.tracks[0].clips[1..] {
        clip.timeline_start -= 10;
    }
    both_measures(&project, &tl, &mut cache);
    assert_eq!(cache.rendered, 3, "a ripple moves the segments, unchanged");

    tl.tracks[0].clips[1].effects.gain_db = vv_core::Keyframed::constant(-6.0);
    both_measures(&project, &tl, &mut cache);
    assert_eq!(cache.rendered, 4, "only the clip edited");
}

#[test]
fn measured_parts_survive_the_disk() {
    let parts = [
        LevelParts::Peak(0.25),
        LevelParts::Loudness {
            segment_frames: 4800,
            segments: vec![0.5, 1.5, 3.0],
            tail: (0.75, 1200),
        },
    ];
    let sources = [Stamp {
        path: PathBuf::from("/media/voice.wav"),
        len: 1234,
        modified_nanos: 5678,
    }];
    for part in parts {
        let bytes = encode_parts(&sources, &part);
        assert_eq!(decode_parts(&bytes), Some((sources.to_vec(), part)));
        assert_eq!(decode_parts(&bytes[..bytes.len() - 1]), None);
        let mut previous = bytes;
        previous[4..8].copy_from_slice(&(PARTS_VERSION - 1).to_le_bytes());
        assert_eq!(decode_parts(&previous), None);
    }
}

/// A normalizing timeline was collected whole until its levels were
/// measured apart.
#[test]
fn a_normalizing_timeline_renders_in_chunks() {
    let mut project = Project::default();
    let file = media(&mut project, chirp("normalized.wav", 48_000, 60), 48_000, 1);
    let mut tl = timeline(vec![audio_track(vec![clip(1, file, 0, 1500, 0)])]);
    tl.master.effects = vec![normalize(NormalizeMode::Loudness)];
    let mut cache = LevelCache::default();
    let mut audio = StreamingAudio::new(&mut cache);
    let snapshot = audio
        .snapshot(&project, Mix::Timeline(&tl), 0..CHUNK_FRAMES, false)
        .unwrap();
    let ClipInput::Blocks(window) = &snapshot.clips[0].input else {
        panic!("a file clip");
    };
    assert!(window.end() < 2 * CHUNK_FRAMES, "{}", window.end());
    assert_eq!(snapshot.master.inserts.len(), 1);
}

/// Reopening a project: the levels measured the last time come from disk.
#[test]
fn a_new_cache_reads_the_measures_from_disk() {
    let dir = std::env::temp_dir().join("vv-audio-render-test-levels");
    let _ = std::fs::remove_dir_all(&dir);
    let on_disk = || LevelCache {
        dir: Some(dir.clone()),
        ..LevelCache::default()
    };
    let mut project = Project::default();
    let mut tl = edited_timeline(&mut project, "reopened.wav");
    tl.master.effects = vec![normalize(NormalizeMode::Loudness)];
    let mut first = on_disk();
    let measured = both_measures(&project, &tl, &mut first);
    assert!(first.rendered > 0);

    let mut reopened = on_disk();
    assert_eq!(both_measures(&project, &tl, &mut reopened), measured);
    assert_eq!(reopened.rendered, 0);
}

/// The project keeps the fingerprint taken at import: the file is checked
/// again when its measure comes from disk.
#[test]
fn a_file_changed_on_disk_is_measured_again() {
    let dir = std::env::temp_dir().join("vv-audio-render-test-changed");
    let _ = std::fs::remove_dir_all(&dir);
    let on_disk = || LevelCache {
        dir: Some(dir.clone()),
        ..LevelCache::default()
    };
    let path = chirp("changed.wav", 48_000, 10);
    let mut project = Project::default();
    let file = media(&mut project, path.clone(), 48_000, 1);
    let mut tl = timeline(vec![audio_track(vec![clip(1, file, 0, 200, 0)])]);
    tl.master.effects = vec![normalize(NormalizeMode::Loudness)];
    let before = both_measures(&project, &tl, &mut on_disk());

    let quieter = "aevalsrc=0.1*sin(300*t*t):sample_rate=48000:duration=10";
    vv_media::test_support::ffmpeg(&["-f", "lavfi", "-i", quieter], &path);
    let mut reopened = on_disk();
    let after = both_measures(&project, &tl, &mut reopened);
    assert!(reopened.rendered > 0);
    assert_eq!(
        after,
        both_measures(&project, &tl, &mut LevelCache::default())
    );
    assert_ne!(after, before);
}

#[test]
fn an_in_memory_measure_is_rechecked_when_its_source_changes() {
    let path = chirp("changed-in-memory.wav", 48_000, 10);
    let mut project = Project::default();
    let file = media(&mut project, path.clone(), 48_000, 1);
    let mut tl = timeline(vec![audio_track(vec![clip(1, file, 0, 200, 0)])]);
    tl.master.effects = vec![normalize(NormalizeMode::Loudness)];

    let mut cache = LevelCache::default();
    let before = both_measures(&project, &tl, &mut cache);
    let rendered_before = cache.rendered;
    assert!(rendered_before > 0);

    let quieter = "aevalsrc=0.1*sin(300*t*t):sample_rate=48000:duration=10";
    vv_media::test_support::ffmpeg(&["-f", "lavfi", "-i", quieter], &path);
    let after = both_measures(&project, &tl, &mut cache);

    assert!(cache.rendered > rendered_before);
    assert_ne!(after, before);
}

fn segment(frames: Range<u64>) -> LevelSegment {
    LevelSegment {
        frames,
        preroll: 0,
        key: 0,
    }
}

#[test]
fn neighbouring_segments_render_as_one_run_split_between_the_threads() {
    let sec = PROJECT_SAMPLE_RATE as u64;
    let segments = [
        segment(0..10 * sec),
        segment(10 * sec..20 * sec),
        segment(21 * sec..30 * sec),
        segment(30 * sec..40 * sec),
        segment(100 * sec..110 * sec),
    ];
    let refs: Vec<&LevelSegment> = segments.iter().collect();
    let lengths = |threads| -> Vec<usize> { runs(&refs, threads).iter().map(Vec::len).collect() };
    assert_eq!(lengths(1), [4, 1], "a short gap is rendered through");
    assert_eq!(lengths(2), [3, 1, 1], "about half each");
}

#[test]
fn a_level_measured_on_several_threads_reads_as_on_one() {
    let mut project = Project::default();
    let mut tl = edited_timeline(&mut project, "threads.wav");
    tl.master.effects = vec![normalize(NormalizeMode::Loudness)];
    let measure = |threads| {
        let mut cache = LevelCache {
            threads,
            ..LevelCache::default()
        };
        both_measures(&project, &tl, &mut cache)[0].0
    };
    let (one, several) = (measure(1), measure(4));
    assert!(db(one, several).abs() < 0.1, "{one} vs {several}");
}

/// Rendered from the files in chunks and stretched as it goes, it equals the
/// whole input stretched at once, a compound clip with an effect inside
/// included.
#[test]
fn a_pitch_corrected_clip_rendered_in_chunks_equals_its_whole_stretch() {
    let mut project = Project::default();
    let file = media(&mut project, chirp("stretch.wav", 48_000, 20), 48_000, 1);
    let mut nested_track = audio_track(vec![clip(1, file, 0, 300, 0)]);
    nested_track.mix.effects = vec![effect(AudioEffectKind::MultibandCompressor(
        MultibandCompressor::DEFAULT,
    ))];
    let nested = project.timelines.insert(timeline(vec![nested_track]));
    let compound = project.media_pool.insert(MediaItem {
        compound: Some(nested),
        ..project.media_pool[file].clone()
    });
    let stretched = |id, media| {
        let mut clip = Clip::new(
            ClipId(id),
            ClipSource::Media(media),
            0,
            10,
            200,
            Rational::one(),
            Rational::new(5, 4),
        );
        clip.pitch_correction = true;
        clip
    };
    let tl = timeline(vec![
        audio_track(vec![stretched(2, file)]),
        audio_track(vec![stretched(3, compound)]),
    ]);
    let range = samples(0..220);
    assert!(samples(0..250).end > CHUNK_FRAMES, "the input spans chunks");
    let whole = mixed_whole(&project, &tl, range.clone());
    assert_eq!(rendered(&project, &tl, range), whole);
    assert!(whole.iter().any(|s| s.abs() > 0.1));
}

#[test]
fn a_compound_measuring_a_segment_its_parent_claimed_does_not_wait_on_itself() {
    let mut project = Project::default();
    let file = media(&mut project, chirp("self_wait.wav", 48_000, 70), 48_000, 1);
    let chain = || {
        vec![
            effect(AudioEffectKind::MultibandCompressor(
                MultibandCompressor::DEFAULT,
            )),
            normalize(NormalizeMode::Loudness),
        ]
    };
    // The same first clip bare and inside the compound: one segment key.
    let mut nested = timeline(vec![audio_track(vec![
        clip(1, file, 0, 150, 0),
        clip(2, file, 200, 1200, 150),
    ])]);
    nested.master.effects = chain();
    let nested = project.timelines.insert(nested);
    let compound = project.media_pool.insert(MediaItem {
        compound: Some(nested),
        ..project.media_pool[file].clone()
    });
    let mut tl = timeline(vec![audio_track(vec![
        clip(1, file, 0, 150, 0),
        clip(2, compound, 0, 1350, 150),
    ])]);
    tl.master.effects = chain();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut cache = LevelCache {
            threads: 8,
            ..LevelCache::default()
        };
        let rendered = render_offline(&project, &tl, samples(0..1500), &mut cache, |_| {
            ControlFlow::Continue(())
        });
        let _ = tx.send(rendered.is_ok());
    });
    let rendered = rx.recv_timeout(std::time::Duration::from_secs(60));
    assert_eq!(rendered, Ok(true), "the render hung or failed");
}

#[test]
fn a_cancelled_render_stops_at_the_next_chunk() {
    let mut project = Project::default();
    let file = media(&mut project, chirp("cancelled.wav", 48_000, 20), 48_000, 1);
    let tl = timeline(vec![audio_track(vec![clip(1, file, 0, 500, 0)])]);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut cache = LevelCache {
        cancel: Some(cancel.clone()),
        ..LevelCache::default()
    };
    let mut chunks = 0;
    let rendered = render_offline(&project, &tl, samples(0..500), &mut cache, |_| {
        chunks += 1;
        cancel.store(true, Ordering::Relaxed);
        ControlFlow::Continue(())
    });
    assert!(matches!(rendered, Err(ExportError::Cancelled)));
    assert_eq!(chunks, 1);
}

#[test]
fn a_decode_thread_gone_without_an_end_fails_the_render() {
    let (tx, decoded) = sync_channel(1);
    drop(tx);
    let mut reader = Reader {
        path: PathBuf::from("gone.wav"),
        streams: vec![0],
        decoded,
        kept: vec![KeptStream {
            first: 0,
            blocks: VecDeque::new(),
            filling: new_block(),
        }],
        ended: false,
        read_from: vec![None],
        used: false,
    };
    assert!(matches!(
        reader.read(0, 0..100),
        Err(ExportError::Failed(_))
    ));
}
