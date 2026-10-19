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

/// Renders with `installed` as the plugin list, not the process-global one.
fn rendered_with_installed(
    project: &Project,
    tl: &Timeline,
    frames: u64,
    installed: &Arc<[vv_plugin::PluginInfo]>,
) -> Vec<f32> {
    let installed = installed.clone();
    let mut levels = LevelCache {
        installed: Arc::new(move || installed.clone()),
        ..LevelCache::default()
    };
    let mut out = Vec::new();
    render_offline(project, tl, 0..frames, &mut levels, |chunk| {
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
    /// The live pool makes every plugin.
    live_plugins: bool,
    /// ... as an `Echo` rather than a pass-through.
    echo: bool,
}

impl AudioSource for BothMeasures<'_> {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        decoded_whole(path, stream).map_or(ClipAudio::Missing, ClipAudio::Ready)
    }

    fn plugin(&mut self, _: vv_audio::PluginRequest<'_>) -> Option<vv_audio::PluginHandle> {
        let echo = self.echo;
        self.live_plugins.then(|| match echo {
            true => Arc::new(Echo::new(Default::default())) as _,
            false => Arc::new(Passthrough) as _,
        })
    }

    fn plugin_build(&mut self, _: &vv_core::Vst3Effect) -> Option<u64> {
        self.live_plugins.then_some(1)
    }

    fn level(&mut self, analysis: LevelAnalysis) -> LevelReading {
        let by_segment = measure_level(&analysis, self.cache).unwrap();
        let whole = analysis.measure_with(&mut Whole {
            live_plugins: self.live_plugins,
            echo: self.echo,
        });
        self.levels.push((by_segment, whole));
        LevelReading::Ready(whole)
    }
}

/// The decoded files, and the plugins of `BothMeasures`.
struct Whole {
    live_plugins: bool,
    echo: bool,
}

impl AudioSource for Whole {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        decoded_whole(path, stream).map_or(ClipAudio::Missing, ClipAudio::Ready)
    }

    fn plugin(&mut self, _: vv_audio::PluginRequest<'_>) -> Option<vv_audio::PluginHandle> {
        let echo = self.echo;
        self.live_plugins.then(|| match echo {
            true => Arc::new(Echo::new(Default::default())) as _,
            false => Arc::new(Passthrough) as _,
        })
    }
}

fn both_measures(project: &Project, tl: &Timeline, cache: &mut LevelCache) -> Vec<(f32, f32)> {
    both_measures_through(project, tl, cache, false)
}

fn both_measures_through(
    project: &Project,
    tl: &Timeline,
    cache: &mut LevelCache,
    live_plugins: bool,
) -> Vec<(f32, f32)> {
    let mut source = BothMeasures {
        cache,
        levels: Vec::new(),
        live_plugins,
        echo: false,
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
    if vv_audio::stretch::skip_without_rubberband() {
        return;
    }
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

use vv_plugin::testing::{
    FIXTURE_CLASS_ID as FIXTURE_CLASS, Passthrough, counting_pool, fixture_dir, fixture_effect,
    fixture_effect_of, fixture_info,
};

fn plugin_effect(class_id: &str, state: &[u8]) -> AudioEffect {
    AudioEffect::new(AudioEffectKind::Vst3(fixture_effect_of(class_id, state)))
}

fn chirp_project(secs: u32) -> (Project, Track) {
    let mut project = Project::default();
    let id = media(&mut project, chirp("plugin.wav", 48_000, secs), 48_000, 1);
    let len = secs as u64 * FPS;
    (project, audio_track(vec![clip(1, id, 0, len, 0)]))
}

fn rendered_samples(project: &Project, tl: &Timeline, frames: u64) -> Vec<f32> {
    rendered(project, tl, 0..frames)
}

#[test]
fn render_with_uninstalled_plugin_equals_without() {
    let (project, plain) = chirp_project(2);
    let mut with = plain.clone();
    with.mix
        .effects
        .push(plugin_effect("00000000000000000000000000000000", &[1, 2]));
    let frames = 2 * PROJECT_SAMPLE_RATE as u64;
    let installed: Arc<[vv_plugin::PluginInfo]> = Arc::from([]);
    let without = rendered_with_installed(&project, &timeline(vec![plain]), frames, &installed);
    let with = rendered_with_installed(&project, &timeline(vec![with]), frames, &installed);
    assert_eq!(with, without);
}

#[test]
fn render_creates_plugins_once_per_render() {
    let (project, mut track) = chirp_project(20);
    track.mix.effects.push(plugin_effect(FIXTURE_CLASS, &[]));
    let (plugins, made) = counting_pool(|| Some(Arc::new(Passthrough)));
    let frames = 2 * CHUNK_FRAMES + 1000;
    let mut levels = LevelCache::default();
    levels.plugins = vec![plugins];
    render(
        &project,
        Mix::Timeline(&timeline(vec![track])),
        0..frames,
        &mut levels,
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    assert_eq!(made.load(Ordering::SeqCst), 1);
}

#[test]
fn export_applies_fixture_gain() {
    let infos = vv_plugin::scan(&[fixture_dir()]);
    assert!(infos.iter().any(|i| i.class_id == FIXTURE_CLASS));
    vv_plugin::set_installed(infos);

    let (project, plain) = chirp_project(2);
    let mut hosted = plain.clone();
    // 0.25 as f32 little-endian: the fixture's gain is twice its state.
    hosted
        .mix
        .effects
        .push(plugin_effect(FIXTURE_CLASS, &0.25f32.to_le_bytes()));
    let mut reference = plain;
    reference.mix.gain_db = -6.0206;
    let frames = 2 * PROJECT_SAMPLE_RATE as u64;
    let hosted = rendered_samples(&project, &timeline(vec![hosted]), frames);
    let reference = rendered_samples(&project, &timeline(vec![reference]), frames);
    assert_eq!(hosted.len(), reference.len());
    assert!(hosted.iter().any(|s| s.abs() > 0.1));
    for (i, (a, b)) in hosted.iter().zip(&reference).enumerate() {
        assert!((a - b).abs() < 1e-5, "sample {i}: {a} vs {b}");
    }
}

const ECHO_FRAMES: usize = 7 * PROJECT_SAMPLE_RATE as usize;

/// One 0.5 tap `ECHO_FRAMES` after the input, so its tail outlives a chunk.
struct Echo(
    std::sync::Mutex<(Vec<f32>, usize)>,
    Arc<std::sync::atomic::AtomicUsize>,
);

impl Echo {
    fn new(calls: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        let len = ECHO_FRAMES * PROJECT_CHANNELS as usize;
        Self(std::sync::Mutex::new((vec![0.0; len], 0)), calls)
    }
}

impl vv_audio::PluginProcessor for Echo {
    fn process(&self, samples: &mut [f32], _: usize) -> bool {
        self.1.fetch_add(1, Ordering::SeqCst);
        let (line, at) = &mut *self.0.lock().unwrap();
        for s in samples {
            let delayed = line[*at];
            line[*at] = *s;
            *s += 0.5 * delayed;
            *at = (*at + 1) % line.len();
        }
        true
    }
    fn reset(&self) {
        let (line, at) = &mut *self.0.lock().unwrap();
        line.fill(0.0);
        *at = 0;
    }
    fn latency(&self) -> u32 {
        0
    }
}

impl vv_plugin::HostedPlugin for Echo {
    fn set_state(&self, _: &vv_core::Vst3Effect) {}
    fn state_hash(&self) -> u64 {
        0
    }
    fn mark_state(&self, _: u64) {}
}

#[test]
fn a_tail_rings_through_a_gap_spanning_chunks_and_no_ghost_follows() {
    let mut project = Project::default();
    let id = media(&mut project, chirp("echo.wav", 48_000, 5), 48_000, 1);
    let mut dry = audio_track(vec![clip(1, id, 0, 25, 0), clip(2, id, 0, 25, 750)]);
    let plain = timeline(vec![dry.clone()]);
    dry.mix.effects.push(plugin_effect(FIXTURE_CLASS, &[]));
    let echoed = timeline(vec![dry]);
    let range = samples(0..875);
    assert!(range.end - range.start > 6 * CHUNK_FRAMES);

    let (plugins, _) = counting_pool(|| Some(Arc::new(Echo::new(Default::default()))));
    let mut chunked = Vec::new();
    let mut levels = LevelCache::default();
    levels.plugins = vec![plugins];
    render(
        &project,
        Mix::Timeline(&echoed),
        range.clone(),
        &mut levels,
        |chunk| {
            chunked.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
    )
    .unwrap();

    let dry = rendered(&project, &plain, range);
    let delay = ECHO_FRAMES * PROJECT_CHANNELS as usize;
    assert_eq!(chunked.len(), dry.len());
    let mut echoes = 0;
    for (i, got) in chunked.iter().enumerate() {
        let tap = i.checked_sub(delay).map_or(0.0, |j| dry[j]);
        let want = dry[i] + 0.5 * tap;
        assert!((got - want).abs() < 1e-6, "sample {i}: {got} vs {want}");
        echoes += (tap != 0.0) as usize;
    }
    assert!(echoes > 0);
}

/// Two short clips 9 s apart, the 7 s tap of the first one falling in the gap,
/// and a clip of another track ending after the tap of the second.
fn echoed_gap(project: &mut Project, name: &str, on_track: bool) -> Timeline {
    let file = media(project, chirp(name, 48_000, 5), 48_000, 1);
    let mut track = audio_track(vec![clip(1, file, 0, 25, 0), clip(2, file, 25, 25, 250)]);
    let mut tl = timeline(Vec::new());
    if on_track {
        track.mix.effects.push(plugin_effect(FIXTURE_CLASS, &[]));
    } else {
        tl.master.effects.push(plugin_effect(FIXTURE_CLASS, &[]));
    }
    tl.tracks.push(track);
    // Ends after the last tap of the first track.
    tl.tracks
        .push(audio_track(vec![clip(3, file, 75, 25, 500)]));
    tl.master.effects.push(normalize(NormalizeMode::Loudness));
    tl
}

fn echo_measures(project: &Project, tl: &Timeline, cache: &mut LevelCache) -> Vec<(f32, f32)> {
    let mut source = BothMeasures {
        cache,
        levels: Vec::new(),
        live_plugins: true,
        echo: true,
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

fn echo_cache() -> LevelCache {
    let mut cache = fixed_list_cache();
    cache.plugins = vec![echo_plugins(Default::default())];
    cache
}

/// The tail of a plugin rings through the gap in the mix: the measure sees
/// it, segmented or not.
#[test]
fn a_level_after_a_plugin_reads_its_tail_in_the_gaps() {
    for on_track in [false, true] {
        let mut project = Project::default();
        let tl = echoed_gap(&mut project, &format!("tail{on_track}.wav"), on_track);
        let levels = echo_measures(&project, &tl, &mut echo_cache());
        let [(by_segment, whole)] = levels[..] else {
            panic!("{levels:?}");
        };
        assert!(
            db(by_segment, whole).abs() < 0.01,
            "on track {on_track}: {by_segment} vs {whole}"
        );
    }
}

/// Export renders to the timeline end: video past the last audio clip lets the
/// tap of the plugin ring into the measure.
#[test]
fn a_plugin_strip_is_measured_to_the_end_of_the_video() {
    let mut project = Project::default();
    let file = media(&mut project, chirp("video-end.wav", 48_000, 5), 48_000, 1);
    let mut strip = audio_track(vec![clip(1, file, 0, 25, 0)]);
    strip.mix.effects.push(plugin_effect(FIXTURE_CLASS, &[]));
    let mut audio_only = timeline(vec![strip]);
    audio_only
        .master
        .effects
        .push(normalize(NormalizeMode::Loudness));
    let mut with_video = audio_only.clone();
    let mut video = Track::new(TrackKind::Video);
    // The Echo tap falls 7 s after the input, past the 1 s audio clip.
    video.clips.push(clip(2, file, 0, 300, 0));
    with_video.tracks.push(video);

    let [(short, _)] = echo_measures(&project, &audio_only, &mut echo_cache())[..] else {
        panic!("one measure");
    };
    let [(long, whole)] = echo_measures(&project, &with_video, &mut echo_cache())[..] else {
        panic!("one measure");
    };
    assert!(db(short, long).abs() > 0.5, "{short} vs {long}");
    assert!(db(long, whole).abs() < 0.01, "{long} vs {whole}");
}

/// A delay longer than the preroll, an edit two clips before: nothing cached
/// from before the edit is reused, and a cold cache reads the same.
#[test]
fn an_edit_before_a_plugin_tail_measures_the_whole_strip_again() {
    let mut project = Project::default();
    let mut tl = echoed_gap(&mut project, "edited-tail.wav", true);
    let mut cache = echo_cache();
    let before = echo_measures(&project, &tl, &mut cache);
    let rendered = cache.rendered;
    assert_eq!(echo_measures(&project, &tl, &mut cache), before);
    assert_eq!(cache.rendered, rendered, "measured already");

    tl.tracks[0].clips[0].effects.gain_db = vv_core::Keyframed::constant(-6.0);
    let warm = echo_measures(&project, &tl, &mut cache);
    assert!(cache.rendered > rendered);
    assert_ne!(warm, before);
    assert_eq!(warm, echo_measures(&project, &tl, &mut echo_cache()));
}

fn echo_plugins(calls: Arc<std::sync::atomic::AtomicUsize>) -> vv_plugin::Instances {
    counting_pool(move || Some(Arc::new(Echo::new(calls.clone())))).0
}

#[test]
fn a_muted_track_with_a_plugin_renders_silence() {
    let mut project = Project::default();
    let id = media(&mut project, chirp("muted.wav", 48_000, 2), 48_000, 1);
    let mut track = audio_track(vec![clip(1, id, 0, 25, 0)]);
    track.mix.effects.push(plugin_effect(FIXTURE_CLASS, &[]));
    track.muted = true;
    let tl = timeline(vec![track]);
    let mut out = Vec::new();
    let mut levels = LevelCache::default();
    levels.plugins = vec![echo_plugins(Default::default())];
    render(
        &project,
        Mix::Timeline(&tl),
        samples(0..300),
        &mut levels,
        |chunk| {
            out.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
    )
    .unwrap();
    assert!(!out.is_empty() && out.iter().all(|s| *s == 0.0));
}

#[test]
fn a_clipless_track_with_a_plugin_is_not_processed() {
    let mut project = Project::default();
    let id = media(&mut project, chirp("clipless.wav", 48_000, 2), 48_000, 1);
    let mut empty = audio_track(Vec::new());
    empty.mix.effects.push(plugin_effect(FIXTURE_CLASS, &[]));
    let tl = timeline(vec![audio_track(vec![clip(1, id, 0, 25, 0)]), empty]);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut levels = LevelCache::default();
    levels.plugins = vec![echo_plugins(calls.clone())];
    render(
        &project,
        Mix::Timeline(&tl),
        samples(0..300),
        &mut levels,
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

fn fake_known() -> Vec<vv_plugin::PluginInfo> {
    vec![fixture_info()]
}

fn fixed_list_cache() -> LevelCache {
    // The installed list is global and other tests set it.
    let list: Arc<[vv_plugin::PluginInfo]> = Arc::from(fake_known());
    let mut levels = LevelCache::default();
    levels.installed = Arc::new(move || list.clone());
    levels
}

fn render_to_vec(
    project: &Project,
    tl: &Timeline,
    range: Range<u64>,
    levels: &mut LevelCache,
) -> Vec<f32> {
    let mut out = Vec::new();
    render(project, Mix::Timeline(tl), range, levels, |chunk| {
        out.extend_from_slice(chunk);
        ControlFlow::Continue(())
    })
    .unwrap();
    out
}

#[test]
fn consecutive_renders_through_a_cache_reuse_its_plugins() {
    let mut project = Project::default();
    let id = media(&mut project, chirp("reuse.wav", 48_000, 2), 48_000, 1);
    let mut track = audio_track(vec![clip(1, id, 0, 25, 0)]);
    track.mix.effects.push(plugin_effect(FIXTURE_CLASS, &[]));
    let tl = timeline(vec![track]);
    let range = samples(0..180);
    let mut levels = fixed_list_cache();
    let (plugins, made) = counting_pool(|| Some(Arc::new(Echo::new(Default::default()))));
    levels.plugins = vec![plugins];
    let first = render_to_vec(&project, &tl, range.clone(), &mut levels);
    assert!(first.iter().any(|s| *s != 0.0));
    for _ in 0..3 {
        // The echo's line is not empty at the end of a render: only a reset
        // makes the next render equal the first.
        let again = render_to_vec(&project, &tl, range.clone(), &mut levels);
        assert_eq!(again, first);
    }
    assert_eq!(made.load(Ordering::SeqCst), 1);
}

#[test]
fn renders_of_different_strips_keep_each_others_instances() {
    let mut project = Project::default();
    let id = media(&mut project, chirp("strips.wav", 48_000, 2), 48_000, 1);
    let strip = |clip_id| {
        let mut track = audio_track(vec![clip(clip_id, id, 0, 25, 0)]);
        let mut plugin = plugin_effect(FIXTURE_CLASS, &[]);
        if let AudioEffectKind::Vst3(vst3) = &mut plugin.kind {
            vst3.id = vv_core::EffectId(clip_id);
        }
        track.mix.effects.push(plugin);
        timeline(vec![track])
    };
    let (a, b) = (strip(1), strip(2));
    let mut levels = fixed_list_cache();
    let (plugins, made) = counting_pool(|| Some(Arc::new(Passthrough)));
    levels.plugins = vec![plugins];
    for _ in 0..3 {
        render_to_vec(&project, &a, samples(0..50), &mut levels);
        render_to_vec(&project, &b, samples(0..50), &mut levels);
    }
    assert_eq!(made.load(Ordering::SeqCst), 2);
}

type Count = Arc<std::sync::atomic::AtomicUsize>;
type NewPool = Arc<dyn Fn(Arc<[vv_plugin::PluginInfo]>) -> Instances + Send + Sync>;

/// Pools whose pass-through instances are counted together.
fn counting_pools() -> (NewPool, Count, Count) {
    let (made, pools) = (Count::default(), Count::default());
    let (counter, pool_counter) = (made.clone(), pools.clone());
    let new_pool: NewPool = Arc::new(move |known| {
        pool_counter.fetch_add(1, Ordering::SeqCst);
        let counter = counter.clone();
        Instances::with_maker(
            known,
            Box::new(move |_, _, _| {
                counter.fetch_add(1, Ordering::SeqCst);
                Some(Arc::new(Passthrough))
            }),
        )
    });
    (new_pool, made, pools)
}

/// A pool holding `n` instances, counted in `made`.
fn pool_of(n: u64, made: &Count) -> Instances {
    let (new_pool, ..) = counting_pools();
    let mut pool = new_pool(Arc::from(fake_known()));
    let effect = fixture_effect(&[]);
    pool.begin_pass();
    for key in 0..n {
        pool.get(vv_audio::PluginRequest {
            key,
            effect: &effect,
            sample_rate: 48_000,
            process: true,
        })
        .unwrap();
    }
    made.fetch_add(n as usize, Ordering::SeqCst);
    pool
}

#[test]
fn each_measure_thread_gets_a_pool_back_and_the_budget_holds() {
    let made = Count::default();
    let mut cache = LevelCache {
        threads: 4,
        ..LevelCache::default()
    };
    let list: Arc<[vv_plugin::PluginInfo]> = Arc::from(fake_known());
    let mut threads: Vec<LevelCache> = (0..4).map(|_| cache.for_thread()).collect();
    for thread in &mut threads {
        thread.plugins.push(pool_of(3, &made));
        thread.known = Some(list.clone());
    }
    for thread in threads {
        cache.absorb(thread);
    }
    assert_eq!(cache.plugins.len(), 4);
    let mut again: Vec<LevelCache> = (0..4).map(|_| cache.for_thread()).collect();
    assert!(again.iter().all(|thread| thread.plugins.len() == 1));
    assert!(cache.plugins.is_empty());
    assert_eq!(made.load(Ordering::SeqCst), 12);

    // A thread that saw another list: its pool is dropped.
    let other: Arc<[vv_plugin::PluginInfo]> = Arc::from(fake_known());
    again[2].known = Some(other);
    for thread in again {
        cache.absorb(thread);
    }
    assert_eq!(cache.plugins.len(), 3);
}

#[test]
fn the_pools_kept_between_renders_stay_within_the_budget() {
    let made = Count::default();
    let mut cache = LevelCache {
        threads: 8,
        ..LevelCache::default()
    };
    for _ in 0..6 {
        cache.keep_pool(pool_of(10, &made));
        let kept: usize = cache.plugins.iter().map(Instances::len).sum();
        assert!(kept <= KEPT_INSTANCES, "{kept}");
    }
    assert_eq!(cache.plugins.len(), 3);
    let mut one = LevelCache {
        threads: 1,
        ..LevelCache::default()
    };
    for _ in 0..4 {
        one.keep_pool(pool_of(1, &made));
    }
    assert_eq!(one.plugins.len(), 2);
}

#[test]
fn a_nested_render_and_its_outer_one_keep_their_pools() {
    let (project, mut tl, mut levels, _) = pool_race(true, "both_pools.wav");
    let list: Arc<[vv_plugin::PluginInfo]> = Arc::from(fake_known());
    levels.installed = Arc::new(move || list.clone());
    let (new_pool, made, pools) = counting_pools();
    levels.plugins.clear();
    levels.new_pool = new_pool;
    let mut render = |state: u8, levels: &mut LevelCache| {
        let AudioEffectKind::Vst3(plugin) = &mut tl.tracks[0].mix.effects[0].kind else {
            unreachable!()
        };
        plugin.component_state = vv_core::PluginState::new(vec![state]);
        render_to_vec(&project, &tl, samples(0..50), levels);
    };
    render(1, &mut levels);
    assert_eq!(pools.load(Ordering::SeqCst), 2, "the measure did not nest");
    let first = made.load(Ordering::SeqCst);
    // A new state is measured again, in a render nested in the outer one.
    for state in 2..5 {
        render(state, &mut levels);
    }
    assert_eq!(pools.load(Ordering::SeqCst), 2);
    assert_eq!(made.load(Ordering::SeqCst), first);
}

#[test]
fn a_failing_plugin_is_attempted_once_across_renders() {
    let mut project = Project::default();
    let id = media(&mut project, chirp("fail.wav", 48_000, 2), 48_000, 1);
    let mut track = audio_track(vec![clip(1, id, 0, 25, 0)]);
    track.mix.effects.push(plugin_effect(FIXTURE_CLASS, &[]));
    let tl = timeline(vec![track]);
    let mut levels = fixed_list_cache();
    let (plugins, tried) = counting_pool(|| None);
    levels.plugins = vec![plugins];
    for _ in 0..3 {
        render_to_vec(&project, &tl, samples(0..50), &mut levels);
    }
    assert_eq!(tried.load(Ordering::SeqCst), 1);
}

#[test]
fn a_rescan_drops_the_pools_kept_between_renders() {
    let mut project = Project::default();
    let id = media(&mut project, chirp("rescan.wav", 48_000, 2), 48_000, 1);
    let mut track = audio_track(vec![clip(1, id, 0, 25, 0)]);
    track.mix.effects.push(plugin_effect(FIXTURE_CLASS, &[]));
    let tl = timeline(vec![track]);
    let list: Arc<std::sync::Mutex<Arc<[vv_plugin::PluginInfo]>>> =
        Arc::new(std::sync::Mutex::new(Arc::from(fake_known())));
    let source = list.clone();
    let mut levels = LevelCache::default();
    levels.installed = Arc::new(move || source.lock().unwrap().clone());
    let made: Made = Arc::default();
    let kept = made.clone();
    let (plugins, _) = counting_pool(move || {
        let instance = Arc::new(Passthrough);
        kept.lock().unwrap().push(instance.clone());
        Some(instance)
    });
    levels.plugins = vec![plugins];
    render_to_vec(&project, &tl, samples(0..50), &mut levels);
    render_to_vec(&project, &tl, samples(0..50), &mut levels);
    assert_eq!(made.lock().unwrap().len(), 1);
    assert_eq!(Arc::strong_count(&made.lock().unwrap()[0]), 2);
    let rescanned: Arc<[vv_plugin::PluginInfo]> = Arc::from(fake_known());
    *list.lock().unwrap() = rescanned;
    render_to_vec(&project, &tl, samples(0..50), &mut levels);
    assert_eq!(Arc::strong_count(&made.lock().unwrap()[0]), 1);
}

type Made = Arc<std::sync::Mutex<Vec<Arc<Passthrough>>>>;

/// A track with a plugin and a normalization, in `plugin_first` order, and a
/// cache whose pool is `P1` with its one instance.
fn pool_race(plugin_first: bool, name: &str) -> (Project, Timeline, LevelCache, Made) {
    let mut project = Project::default();
    let id = media(&mut project, chirp(name, 48_000, 2), 48_000, 1);
    let mut track = audio_track(vec![clip(1, id, 0, 25, 0)]);
    let plugin = plugin_effect(FIXTURE_CLASS, &[]);
    let normalized = normalize(NormalizeMode::SamplePeak);
    track.mix.effects = if plugin_first {
        vec![plugin, normalized]
    } else {
        vec![normalized, plugin]
    };
    let tl = timeline(vec![track]);
    let (first, second): (Arc<[_]>, Arc<[_]>) = (Arc::from(fake_known()), Arc::from(fake_known()));
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let mut levels = LevelCache {
        threads: 1,
        ..LevelCache::default()
    };
    levels.installed = Arc::new(move || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            first.clone()
        } else {
            second.clone()
        }
    });
    let made: Made = Arc::default();
    let kept = made.clone();
    let (pool, _) = counting_pool(move || {
        let instance = Arc::new(Passthrough);
        kept.lock().unwrap().push(instance.clone());
        Some(instance)
    });
    levels.plugins = vec![pool];
    (project, tl, levels, made)
}

#[test]
fn a_pool_from_a_list_a_nested_render_replaced_is_dropped() {
    let (project, tl, mut levels, made) = pool_race(false, "stale_pool.wav");
    render_to_vec(&project, &tl, samples(0..50), &mut levels);
    let made = made.lock().unwrap();
    assert_eq!(made.len(), 1);
    assert_eq!(Arc::strong_count(&made[0]), 1, "the stale pool was kept");
    assert!(levels.plugins.is_empty());
}

#[test]
fn a_nested_render_does_not_discard_the_outer_pool() {
    let (project, tl, mut levels, made) = pool_race(true, "outer_pool.wav");
    let list: Arc<[vv_plugin::PluginInfo]> = Arc::from(fake_known());
    levels.installed = Arc::new(move || list.clone());
    render_to_vec(&project, &tl, samples(0..50), &mut levels);
    let made = made.lock().unwrap();
    assert_eq!(made.len(), 1);
    assert_eq!(Arc::strong_count(&made[0]), 2, "the outer pool was dropped");
}

fn level_files(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |entries| entries.count())
}

/// A cache on disk in a fresh temp dir, with `plugins` as its pool.
fn disk_cache(name: &str, plugins: Instances) -> (LevelCache, PathBuf) {
    let dir = std::env::temp_dir().join(format!("vv-levels-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut levels = fixed_list_cache();
    levels.dir = Some(dir.clone());
    levels.plugins = vec![plugins];
    (levels, dir)
}

fn normalized_after_plugin(project: &mut Project, nested: bool, name: &str) -> Timeline {
    let file = media(project, chirp(name, 48_000, 4), 48_000, 1);
    let mut track = audio_track(vec![clip(1, file, 0, 100, 0)]);
    track.mix.effects.push(plugin_effect(FIXTURE_CLASS, &[]));
    if !nested {
        let mut tl = timeline(vec![track]);
        tl.master.effects = vec![normalize(NormalizeMode::SamplePeak)];
        return tl;
    }
    let inner = project.timelines.insert(timeline(vec![track]));
    let compound = project.media_pool.insert(MediaItem {
        compound: Some(inner),
        ..project.media_pool[file].clone()
    });
    let mut tl = timeline(vec![audio_track(vec![clip(2, compound, 0, 100, 0)])]);
    tl.master.effects = vec![normalize(NormalizeMode::SamplePeak)];
    tl
}

#[test]
fn a_level_measured_while_a_plugin_failed_is_not_kept_for_a_working_render() {
    for nested in [false, true] {
        let mut project = Project::default();
        let tl = normalized_after_plugin(&mut project, nested, &format!("degraded{nested}.wav"));
        let (failing, _) = counting_pool(|| None);
        let (mut levels, dir) = disk_cache(&format!("fail{nested}"), failing);

        both_measures_through(&project, &tl, &mut levels, true);
        assert!(levels.degraded, "nested {nested}");
        assert_eq!(level_files(&dir), 0, "nested {nested}");
        let rendered = levels.rendered;
        assert!(rendered > 0);

        let (working, _) = counting_pool(|| Some(Arc::new(Passthrough)));
        levels.plugins = vec![working];
        levels.degraded = false;
        both_measures_through(&project, &tl, &mut levels, true);
        assert!(
            levels.rendered > rendered,
            "nested {nested}: measured again"
        );
        assert!(!levels.degraded, "nested {nested}");
        assert!(level_files(&dir) > 0, "nested {nested}");

        let rendered = levels.rendered;
        both_measures_through(&project, &tl, &mut levels, true);
        assert_eq!(levels.rendered, rendered, "nested {nested}: found");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_bypassed_plugin_does_not_degrade_a_measure() {
    let mut project = Project::default();
    let mut tl = normalized_after_plugin(&mut project, false, "bypassed.wav");
    tl.tracks[0].mix.effects[0].enabled = false;
    let (failing, _) = counting_pool(|| None);
    let (mut levels, dir) = disk_cache("bypassed", failing);
    both_measures_through(&project, &tl, &mut levels, true);
    assert!(!levels.degraded);
    assert!(level_files(&dir) > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Halves the audio.
struct Halve;

impl vv_audio::PluginProcessor for Halve {
    fn process(&self, samples: &mut [f32], _: usize) -> bool {
        samples.iter_mut().for_each(|s| *s *= 0.5);
        true
    }
    fn reset(&self) {}
    fn latency(&self) -> u32 {
        0
    }
}

impl vv_plugin::HostedPlugin for Halve {
    fn set_state(&self, _: &vv_core::Vst3Effect) {}
    fn state_hash(&self) -> u64 {
        0
    }
    fn mark_state(&self, _: u64) {}
}

#[test]
fn a_measure_plays_a_plugin_the_live_pool_did_not_make_as_its_key_says() {
    let mut project = Project::default();
    let tl = normalized_after_plugin(&mut project, false, "reverse.wav");
    // `BothMeasures` keys the segments with no plugin made.
    let (failing, _) = counting_pool(|| None);
    let (mut dry, dry_dir) = disk_cache("reverse-dry", failing);
    let (halving, _) = counting_pool(|| Some(Arc::new(Halve)));
    let (mut wet, wet_dir) = disk_cache("reverse-wet", halving);
    let expected = both_measures(&project, &tl, &mut dry)[0].0;
    let measured = both_measures(&project, &tl, &mut wet)[0].0;
    assert!(expected > 0.1);
    assert_eq!(measured, expected);
    assert!(!wet.degraded);
    let _ = std::fs::remove_dir_all(dry_dir);
    let _ = std::fs::remove_dir_all(wet_dir);
}

#[test]
fn a_plugin_that_is_not_installed_does_not_degrade_a_measure() {
    let mut project = Project::default();
    let mut tl = normalized_after_plugin(&mut project, false, "uninstalled.wav");
    tl.tracks[0].mix.effects[0] = plugin_effect("00000000000000000000000000000000", &[]);
    let (pool, _) = counting_pool(|| None);
    let (mut levels, dir) = disk_cache("uninstalled", pool);
    both_measures(&project, &tl, &mut levels);
    assert!(!levels.degraded);
    assert!(level_files(&dir) > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_measure_inside_a_compound_plays_unmade_plugins_as_its_key_says() {
    let mut project = Project::default();
    let file = media(
        &mut project,
        chirp("reverse-nested.wav", 48_000, 4),
        48_000,
        1,
    );
    let mut track = audio_track(vec![clip(1, file, 0, 100, 0)]);
    track.mix.effects.push(plugin_effect(FIXTURE_CLASS, &[]));
    let mut inner = timeline(vec![track]);
    inner.master.effects = vec![normalize(NormalizeMode::SamplePeak)];
    let inner = project.timelines.insert(inner);
    let compound = project.media_pool.insert(MediaItem {
        compound: Some(inner),
        ..project.media_pool[file].clone()
    });
    let tl = timeline(vec![audio_track(vec![clip(2, compound, 0, 100, 0)])]);
    let (failing, _) = counting_pool(|| None);
    let (mut dry, dry_dir) = disk_cache("nested-dry", failing);
    let (halving, _) = counting_pool(|| Some(Arc::new(Halve)));
    let (mut wet, wet_dir) = disk_cache("nested-wet", halving);
    let expected = both_measures(&project, &tl, &mut dry)[0].0;
    let measured = both_measures(&project, &tl, &mut wet)[0].0;
    assert!(expected > 0.1);
    assert_eq!(measured, expected);
    let _ = std::fs::remove_dir_all(dry_dir);
    let _ = std::fs::remove_dir_all(wet_dir);
}

/// An echo inside a compound clip remembers beyond the preroll as well.
#[test]
fn a_level_after_a_compound_with_a_plugin_reads_its_tail_in_the_gaps() {
    let mut project = Project::default();
    let inner = echoed_gap(&mut project, "compound-tail.wav", true);
    let mut inner = inner;
    inner.master.effects.clear();
    let file = inner.tracks[0].clips[0].source.clone();
    let ClipSource::Media(file) = file else {
        panic!("a media clip");
    };
    let inner_id = project.timelines.insert(inner);
    let compound = project.media_pool.insert(MediaItem {
        compound: Some(inner_id),
        ..project.media_pool[file].clone()
    });
    // Starts inside the compound, over 2 s after the source of its tap.
    let mut tl = timeline(vec![
        audio_track(vec![clip(9, compound, 0, 450, 0)]),
        audio_track(vec![clip(10, file, 0, 25, 425)]),
    ]);
    tl.master.effects = vec![normalize(NormalizeMode::Loudness)];
    let mut cache = echo_cache();
    cache.plugins = vec![echo_plugins(Default::default())];
    let levels = echo_measures(&project, &tl, &mut cache);
    let [(by_segment, whole)] = levels[..] else {
        panic!("{levels:?}");
    };
    assert!(
        db(by_segment, whole).abs() < 0.01,
        "{by_segment} vs {whole}"
    );
}

/// Stops the cache it renders for the first time it processes.
struct Stopper(Arc<AtomicBool>);

impl vv_audio::PluginProcessor for Stopper {
    fn process(&self, _: &mut [f32], _: usize) -> bool {
        self.0.store(true, Ordering::SeqCst);
        true
    }
    fn reset(&self) {}
    fn latency(&self) -> u32 {
        0
    }
}

impl vv_plugin::HostedPlugin for Stopper {
    fn set_state(&self, _: &vv_core::Vst3Effect) {}
    fn state_hash(&self) -> u64 {
        0
    }
    fn mark_state(&self, _: u64) {}
}

/// Measures the first level asked for through the cache, keeping the result.
struct Outer<'c> {
    cache: &'c mut LevelCache,
    result: Option<Result<f32, ExportError>>,
}

impl AudioSource for Outer<'_> {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        decoded_whole(path, stream).map_or(ClipAudio::Missing, ClipAudio::Ready)
    }

    fn plugin(&mut self, _: vv_audio::PluginRequest<'_>) -> Option<vv_audio::PluginHandle> {
        Some(Arc::new(Passthrough))
    }

    fn level(&mut self, analysis: LevelAnalysis) -> LevelReading {
        if self.result.is_none() {
            self.result = Some(measure_level(&analysis, self.cache));
        }
        LevelReading::Ready(1.0)
    }
}

/// A measure stopped inside another one is neither stored nor taken for a level.
#[test]
fn a_nested_measure_that_is_stopped_stores_nothing() {
    let mut project = Project::default();
    let file = media(&mut project, chirp("stopped.wav", 48_000, 4), 48_000, 1);
    let mut inner = timeline(vec![audio_track(vec![clip(1, file, 0, 100, 0)])]);
    inner.tracks[0]
        .mix
        .effects
        .push(plugin_effect(FIXTURE_CLASS, &[]));
    inner.master.effects = vec![normalize(NormalizeMode::SamplePeak)];
    let inner = project.timelines.insert(inner);
    let compound = project.media_pool.insert(MediaItem {
        compound: Some(inner),
        ..project.media_pool[file].clone()
    });
    let mut tl = timeline(vec![audio_track(vec![clip(2, compound, 0, 100, 0)])]);
    tl.master.effects = vec![normalize(NormalizeMode::SamplePeak)];

    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let (plugins, _) = counting_pool(move || Some(Arc::new(Stopper(flag.clone()))));
    let (mut cache, dir) = disk_cache("stopped", plugins);
    cache.cancel = Some(stop);
    let mut source = Outer {
        cache: &mut cache,
        result: None,
    };
    MixSnapshot::from_timeline(
        &project,
        &tl,
        PROJECT_SAMPLE_RATE,
        PROJECT_CHANNELS,
        &mut source,
    );
    let result = source.result.expect("a level was asked for");
    assert!(matches!(result, Err(ExportError::Cancelled)), "{result:?}");
    assert_eq!(cache.rendered, 0);
    assert_eq!(level_files(&dir), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Records where the segments of each level end.
struct SegmentEnds<'c> {
    cache: &'c mut LevelCache,
    ends: Vec<u64>,
}

impl AudioSource for SegmentEnds<'_> {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        decoded_whole(path, stream).map_or(ClipAudio::Missing, ClipAudio::Ready)
    }

    fn plugin(&mut self, _: vv_audio::PluginRequest<'_>) -> Option<vv_audio::PluginHandle> {
        Some(Arc::new(Echo::new(Default::default())))
    }

    fn level(&mut self, analysis: LevelAnalysis) -> LevelReading {
        measure_level(&analysis, self.cache).unwrap();
        self.ends
            .extend(analysis.segments.last().map(|s| s.frames.end));
        LevelReading::Ready(1.0)
    }
}

/// A track measured after its plugin rings until the mix ends, not until
/// its own last clip does.
#[test]
fn a_track_level_after_a_plugin_runs_to_the_end_of_the_mix() {
    let mut project = Project::default();
    let mut tl = echoed_gap(&mut project, "track-tail.wav", true);
    tl.master.effects.clear();
    tl.tracks[0]
        .mix
        .effects
        .push(normalize(NormalizeMode::Loudness));
    let mut source = SegmentEnds {
        cache: &mut echo_cache(),
        ends: Vec::new(),
    };
    MixSnapshot::from_timeline(
        &project,
        &tl,
        PROJECT_SAMPLE_RATE,
        PROJECT_CHANNELS,
        &mut source,
    );
    let end = timeline_frame_to_sample(525, 25.0, PROJECT_SAMPLE_RATE);
    assert!(!source.ends.is_empty());
    assert!(source.ends.iter().all(|&e| e == end), "{:?}", source.ends);
}
