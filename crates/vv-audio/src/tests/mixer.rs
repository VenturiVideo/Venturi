use super::*;
use std::path::PathBuf;
use vv_core::{Clip, ClipId, Interpolation, MediaItem, MediaMeta, Rational, Track, TrackKind};

const RATE: u32 = 100;

/// Ids unique like a project's: a clip measured alone is found by its id.
fn clip_at(media: vv_core::MediaId, start: FrameIdx, source_in: FrameIdx, len: FrameIdx) -> Clip {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    Clip::from_source_range(
        ClipId(NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)),
        ClipSource::Media(media),
        source_in,
        source_in + len,
        start,
        Rational::one(),
    )
}

/// Project at 10 fps with two media (`a.wav`, `b.wav`): at `RATE` = 100
/// every timeline frame is 10 audio frames.
fn project() -> (Project, vv_core::MediaId, vv_core::MediaId) {
    let mut project = Project::default();
    let meta = MediaMeta {
        duration_frames: 100,
        fps: Rational::new(10, 1),
        width: 0,
        height: 0,
        has_video: true,
        has_audio: true,
        sample_rate: RATE,
        channels: 1,
        audio_streams: 1,
        file: Default::default(),
    };
    let a = project.media_pool.insert(MediaItem {
        path: PathBuf::from("a.wav"),
        meta: meta.clone(),
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let b = project.media_pool.insert(MediaItem {
        path: PathBuf::from("b.wav"),
        meta,
        content_hash: 2,
        compound: None,
        folder: None,
    });
    (project, a, b)
}

fn timeline(tracks: Vec<Track>) -> Timeline {
    Timeline {
        name: "t".into(),
        fps: Rational::new(10, 1),
        resolution: (4, 2),
        tracks,
        markers: Vec::new(),
        master: Default::default(),
    }
}

fn audio_track(clips: Vec<Clip>) -> Track {
    Track {
        kind: TrackKind::Audio,
        clips,
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }
}

/// Mono: `a` is always 0.5, `b` is a ramp (sample i = i/1000);
/// stream 1 of `a` is 0.25.
fn mono(samples: Arc<Vec<f32>>) -> SampleBlocks {
    SampleBlocks::contiguous(samples, 0, 1)
}

fn buffers(path: &Path, stream: usize) -> Option<SampleBlocks> {
    samples(path, stream).map(mono)
}

/// `samples` as stereo frames.
fn stereo(path: &Path, stream: usize) -> Option<SampleBlocks> {
    samples(path, stream).map(|s| SampleBlocks::contiguous(s, 0, 2))
}

fn samples(path: &Path, stream: usize) -> Option<Arc<Vec<f32>>> {
    match (path.to_str()?, stream) {
        ("a.wav", 0) => Some(Arc::new(vec![0.5; 1000])),
        ("a.wav", 1) => Some(Arc::new(vec![0.25; 1000])),
        ("b.wav", 0) => Some(Arc::new((0..1000).map(|i| i as f32 / 1000.0).collect())),
        _ => None,
    }
}

fn render(project: &Project, tl: &Timeline, start: u64, frames: usize) -> Vec<f32> {
    let snap = MixSnapshot::from_timeline(project, tl, RATE, 1, &mut buffers);
    let mut out = vec![9.0; frames];
    mix_range(&snap, start, &mut out);
    out
}

#[test]
fn gap_is_silence() {
    let (project, a, _) = project();
    let tl = timeline(vec![audio_track(vec![clip_at(a, 5, 0, 2)])]);
    let out = render(&project, &tl, 0, 50);
    assert!(out.iter().all(|&s| s == 0.0));
    let empty = timeline(vec![]);
    assert!(render(&project, &empty, 0, 10).iter().all(|&s| s == 0.0));
}

#[test]
fn offset_clip_plays_its_source_range_at_its_timeline_position() {
    let (project, _, b) = project();
    // Timeline frame 2 (= audio 20) plays the source from frame 3 (= audio 30).
    let tl = timeline(vec![audio_track(vec![clip_at(b, 2, 3, 1)])]);
    let out = render(&project, &tl, 15, 20);
    assert!(out[..5].iter().all(|&s| s == 0.0));
    for i in 0..10 {
        assert_eq!(out[5 + i], (30 + i) as f32 / 1000.0);
    }
    assert!(out[15..].iter().all(|&s| s == 0.0));
}

#[test]
fn overlapping_clips_on_different_tracks_are_summed() {
    let (project, a, b) = project();
    let tl = timeline(vec![
        audio_track(vec![clip_at(a, 0, 0, 5)]),
        audio_track(vec![clip_at(b, 0, 0, 5)]),
    ]);
    let out = render(&project, &tl, 10, 5);
    for (i, s) in out.iter().enumerate() {
        assert_eq!(*s, 0.5 + (10 + i) as f32 / 1000.0);
    }
}

#[test]
fn muted_track_is_excluded() {
    let (project, a, b) = project();
    let mut muted = audio_track(vec![clip_at(a, 0, 0, 5)]);
    muted.muted = true;
    let tl = timeline(vec![muted, audio_track(vec![clip_at(b, 0, 0, 5)])]);
    let out = render(&project, &tl, 0, 5);
    for (i, s) in out.iter().enumerate() {
        assert_eq!(*s, i as f32 / 1000.0);
    }
}

#[test]
fn only_solo_tracks_play_when_any_is_solo() {
    let (project, a, b) = project();
    let mut solo = audio_track(vec![clip_at(b, 0, 0, 5)]);
    solo.solo = true;
    let tl = timeline(vec![audio_track(vec![clip_at(a, 0, 0, 5)]), solo]);
    let out = render(&project, &tl, 0, 5);
    for (i, s) in out.iter().enumerate() {
        assert_eq!(*s, i as f32 / 1000.0);
    }
}

#[test]
fn disabled_clip_is_excluded() {
    let (project, a, b) = project();
    let mut disabled = clip_at(a, 0, 0, 5);
    disabled.disabled = true;
    let tl = timeline(vec![
        audio_track(vec![disabled]),
        audio_track(vec![clip_at(b, 0, 0, 5)]),
    ]);
    let out = render(&project, &tl, 0, 5);
    for (i, s) in out.iter().enumerate() {
        assert_eq!(*s, i as f32 / 1000.0);
    }
}

#[test]
fn video_tracks_are_ignored() {
    let (project, a, _) = project();
    let mut video = audio_track(vec![clip_at(a, 0, 0, 5)]);
    video.kind = TrackKind::Video;
    let tl = timeline(vec![video]);
    assert!(render(&project, &tl, 0, 20).iter().all(|&s| s == 0.0));
}

/// `project()` plus a compound clip whose nested timeline plays `a.wav`
/// for 5 frames.
fn project_with_compound() -> (Project, vv_core::MediaId) {
    let (mut project, a, _) = project();
    let nested = project
        .timelines
        .insert(timeline(vec![audio_track(vec![clip_at(a, 0, 0, 5)])]));
    let compound = project.media_pool.insert(MediaItem {
        path: PathBuf::from("Compound Clip 1"),
        meta: project.media_pool[a].meta.clone(),
        content_hash: 10,
        compound: Some(nested),
        folder: None,
    });
    (project, compound)
}

/// Answers from `buffers`, counting the calls.
#[derive(Default)]
struct CountingSource {
    files_asked: Vec<PathBuf>,
}

impl AudioSource for CountingSource {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        self.files_asked.push(path.to_path_buf());
        buffers(path, stream).map_or(ClipAudio::Missing, ClipAudio::Ready)
    }
}

#[test]
fn a_compound_clip_plays_its_nested_timeline_and_never_asks_for_its_own_file() {
    let (project, compound) = project_with_compound();
    let tl = timeline(vec![audio_track(vec![clip_at(compound, 0, 0, 5)])]);
    let mut source = CountingSource::default();
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut source);
    assert_eq!(source.files_asked, [PathBuf::from("a.wav")]);
    let mut out = vec![9.0; 60];
    mix_range(&snap, 0, &mut out);
    assert!(out[..50].iter().all(|&s| s == 0.5));
    assert!(out[50..].iter().all(|&s| s == 0.0));
}

#[test]
fn a_compound_used_twice_plays_at_both_places() {
    let (project, compound) = project_with_compound();
    let tl = timeline(vec![audio_track(vec![
        clip_at(compound, 0, 0, 5),
        clip_at(compound, 10, 0, 5),
    ])]);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
    let mut out = vec![9.0; 150];
    mix_range(&snap, 0, &mut out);
    assert!(out[..50].iter().chain(&out[100..]).all(|&s| s == 0.5));
    assert!(out[50..100].iter().all(|&s| s == 0.0));
}

/// Safety net: `would_create_a_cycle` prevents it, but a compound containing
/// itself must end silent, not overflow the stack.
#[test]
fn a_compound_containing_itself_is_silent() {
    let (mut project, compound) = project_with_compound();
    let nested = project.media_pool[compound].compound.unwrap();
    project.timelines[nested]
        .tracks
        .push(audio_track(vec![clip_at(compound, 0, 0, 5)]));
    let tl = timeline(vec![audio_track(vec![clip_at(compound, 0, 0, 5)])]);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
    assert!(snap.clips.is_empty());
}

/// The bug case: media at 9.99 fps (10000/1001, the small-scale analogue
/// of 59.94 on 60) on a 10 fps timeline. Conformed, the clip
/// lasts on the timeline as long as its audio does, and the mix at the end of the clip is
/// still aligned to the right sample instead of being cut off.
#[test]
fn a_conformed_clip_lasts_as_long_as_its_audio_and_does_not_drift() {
    const SOURCE_FRAMES: FrameIdx = 1000;
    const AUDIO_SAMPLES: usize = 10_010; // 1000 frames / 9.99 fps = 100.1 s

    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path: PathBuf::from("slow.wav"),
        meta: MediaMeta {
            duration_frames: SOURCE_FRAMES,
            fps: Rational::new(10_000, 1001),
            width: 0,
            height: 0,
            has_video: true,
            has_audio: true,
            sample_rate: RATE,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 7,
        compound: None,
        folder: None,
    });
    let rate = Rational::conform_rate(Rational::new(10, 1), Rational::new(10_000, 1001));
    let clip = Clip::from_source_range(
        ClipId(0),
        ClipSource::Media(media),
        0,
        SOURCE_FRAMES,
        0,
        rate,
    );
    assert_eq!(clip.timeline_len, 1001, "100,1 s a 10 fps");
    let tl = timeline(vec![audio_track(vec![clip])]);

    let buffer: Arc<Vec<f32>> =
        Arc::new((0..AUDIO_SAMPLES).map(|i| i as f32 / 100_000.0).collect());
    let buffer_for = |path: &Path, _stream: usize| {
        (path.to_str() == Some("slow.wav")).then(|| mono(buffer.clone()))
    };
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut |p: &Path, s: usize| {
        buffer_for(p, s)
    });
    assert_eq!(snap.clips.len(), 1);
    assert_eq!(
        snap.clips[0].len, AUDIO_SAMPLES as u64,
        "all of the media audio fits in the clip, nothing cut"
    );

    // Last 10 samples of the clip: still the ones at the end of the buffer,
    // no drift accumulated over the preceding 100 s.
    let mut out = vec![9.0; 10];
    mix_range(&snap, AUDIO_SAMPLES as u64 - 10, &mut out);
    for (i, s) in out.iter().enumerate() {
        let expected = (AUDIO_SAMPLES - 10 + i) as f32 / 100_000.0;
        assert!(
            (s - expected).abs() < 1e-6,
            "campione {i}: {s} != {expected}"
        );
    }
}

/// A split in the middle of a source frame does not move the audio: the right
/// half restarts from the sample the original was playing at that point.
#[test]
fn splitting_a_conformed_clip_mid_source_frame_keeps_every_sample() {
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path: PathBuf::from("slow.wav"),
        meta: MediaMeta {
            duration_frames: 1000,
            fps: Rational::new(10_000, 1001),
            width: 0,
            height: 0,
            has_video: true,
            has_audio: true,
            sample_rate: RATE,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 7,
        compound: None,
        folder: None,
    });
    let rate = Rational::conform_rate(Rational::new(10, 1), Rational::new(10_000, 1001));
    let clip = Clip::from_source_range(ClipId(1), ClipSource::Media(media), 0, 1000, 0, rate);
    assert_eq!(
        clip.source_frame_at(500),
        clip.source_frame_at(499),
        "500 is mid-frame"
    );
    let timeline_id = project
        .timelines
        .insert(timeline(vec![audio_track(vec![clip])]));

    let buffer: Arc<Vec<f32>> = Arc::new((0..10_010).map(|i| i as f32 / 100_000.0).collect());
    let buffer_for = |path: &Path, _stream: usize| {
        (path.to_str() == Some("slow.wav")).then(|| mono(buffer.clone()))
    };
    let mix_all = |project: &Project| {
        let timeline = &project.timelines[timeline_id];
        let snap =
            MixSnapshot::from_timeline(project, timeline, RATE, 1, &mut |p: &Path, s: usize| {
                buffer_for(p, s)
            });
        let mut out = vec![0.0; 10_010];
        mix_range(&snap, 0, &mut out);
        out
    };
    let before = mix_all(&project);

    let mut split = vv_core::SplitClip::new(timeline_id, 0, ClipId(1), 500);
    vv_core::Command::apply(&mut split, &mut project);
    assert_eq!(project.timelines[timeline_id].tracks[0].clips.len(), 2);

    assert_eq!(mix_all(&project), before);
}

#[test]
fn constant_gain_scales_the_clip() {
    let (project, a, _) = project();
    let mut clip = clip_at(a, 0, 0, 5);
    clip.effects.gain_db = Keyframed::constant(-6.0206);
    let tl = timeline(vec![audio_track(vec![clip])]);
    for s in render(&project, &tl, 0, 10) {
        assert!((s - 0.25).abs() < 1e-3, "s={s}");
    }
}

#[test]
fn keyframed_gain_is_evaluated_per_block_at_the_source_frame() {
    let (project, a, _) = project();
    // Long enough to cover several gain blocks.
    let blocks = 3;
    let len_frames = (GAIN_BLOCK_FRAMES * blocks) as usize;
    let mut clip = clip_at(a, 0, 0, (len_frames / 10) as FrameIdx);
    let mut gain = Keyframed::constant(0.0f32);
    gain.upsert(0, 0.0, Interpolation::Hold);
    // From the second block (audio 800 = source frame 80) on: practically -inf.
    gain.upsert(80, -200.0, Interpolation::Hold);
    clip.effects.gain_db = gain;
    let tl = timeline(vec![audio_track(vec![clip])]);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut |_: &Path, _: usize| {
        Some(mono(Arc::new(vec![0.5; len_frames])))
    });
    let mut out = vec![0.0; len_frames];
    mix_range(&snap, 0, &mut out);
    let block = GAIN_BLOCK_FRAMES as usize;
    assert!(out[..block].iter().all(|&s| s == 0.5));
    assert!(out[block..].iter().all(|&s| s.abs() < 1e-6));
}

#[test]
fn audio_stream_index_selects_the_buffer() {
    let (project, a, _) = project();
    let mut clip = clip_at(a, 0, 0, 5);
    clip.audio_stream_index = 1;
    let tl = timeline(vec![audio_track(vec![clip])]);
    assert!(render(&project, &tl, 0, 10).iter().all(|&s| s == 0.25));
}

#[test]
fn clip_whose_buffer_is_not_ready_is_silent() {
    let (project, a, b) = project();
    let tl = timeline(vec![
        audio_track(vec![clip_at(a, 0, 0, 5)]),
        audio_track(vec![clip_at(b, 0, 0, 5)]),
    ]);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut |p: &Path, s: usize| {
        (p != Path::new("b.wav")).then(|| buffers(p, s)).flatten()
    });
    let mut out = vec![0.0; 10];
    mix_range(&snap, 0, &mut out);
    assert!(out.iter().all(|&s| s == 0.5));
}

#[test]
fn clip_longer_than_its_buffer_is_clamped() {
    let (project, a, _) = project();
    let tl = timeline(vec![audio_track(vec![clip_at(a, 0, 95, 20)])]);
    let out = render(&project, &tl, 0, 100);
    assert!(out[..50].iter().all(|&s| s == 0.5));
    assert!(out[50..].iter().all(|&s| s == 0.0));
}

#[test]
fn mix_is_independent_of_how_the_range_is_split() {
    let (project, a, b) = project();
    let mut ca = clip_at(a, 1, 0, 30);
    let mut kf = Keyframed::constant(0.0f32);
    kf.upsert(0, 0.0, Interpolation::Linear);
    kf.upsert(30, -12.0, Interpolation::Linear);
    ca.effects.gain_db = kf;
    let tl = timeline(vec![
        audio_track(vec![ca]),
        audio_track(vec![clip_at(b, 7, 2, 20)]),
    ]);
    let whole = render(&project, &tl, 0, 400);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
    let mut chunked = vec![0.0; 400];
    for (i, chunk) in chunked.chunks_mut(37).enumerate() {
        mix_range(&snap, (i * 37) as u64, chunk);
    }
    assert_eq!(whole, chunked);
}

#[test]
fn stereo_mix_keeps_channels_interleaved() {
    let (project, a, _) = project();
    let tl = timeline(vec![audio_track(vec![clip_at(a, 1, 0, 1)])]);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 2, &mut |_: &Path, _: usize| {
        Some(SampleBlocks::contiguous(
            Arc::new([0.1, 0.9].repeat(100)),
            0,
            2,
        ))
    });
    let mut out = vec![0.0; 40];
    mix_range(&snap, 5, &mut out);
    assert!(out[..10].iter().all(|&s| s == 0.0));
    for f in out[10..30].chunks(2) {
        assert_eq!(f, [0.1, 0.9]);
    }
    assert!(out[30..].iter().all(|&s| s == 0.0));
}

#[test]
fn mixer_seek_and_snapshot_swaps_do_not_reopen_or_leak() {
    let mut mixer = Mixer::new().unwrap();
    mixer.seek(12_345);
    assert_eq!(
        mixer.position(),
        12_345,
        "paused, the position does not advance"
    );
    for _ in 0..50 {
        let mix = Arc::new(MixSnapshot::empty(mixer.sample_rate(), mixer.channels()));
        mixer.set_state(Arc::new(MixerState::new(mix, None)));
    }
    assert!(
        mixer.retained.len() <= 3,
        "retained={}",
        mixer.retained.len()
    );
}

fn window(tempo: u64, origin: u64, chunks: &[&[f32]]) -> StretchedWindow {
    StretchedWindow {
        tempo,
        origin,
        chunks: chunks.iter().map(|c| Arc::new(c.to_vec())).collect(),
    }
}

#[test]
fn stretched_window_maps_timeline_position_to_stretched_frames() {
    let w = window(4, 100, &[&[1.0, 2.0, 3.0], &[4.0, 5.0]]);
    assert_eq!(w.covered_until(1), 120);

    let mut out = [9.0; 3];
    render_stretched(&w, 1, 100, &mut out);
    assert_eq!(out, [1.0, 2.0, 3.0]);

    // Position 110 = stretched frame 2, across the chunk boundary.
    render_stretched(&w, 1, 110, &mut out);
    assert_eq!(out, [3.0, 4.0, 5.0]);

    render_stretched(&w, 1, 116, &mut out);
    assert_eq!(out, [5.0, 0.0, 0.0], "past the end: silence");
    render_stretched(&w, 1, 50, &mut out);
    assert_eq!(out, [0.0; 3], "before the origin: silence");
}

#[test]
fn stretched_window_keeps_stereo_frames_aligned() {
    let w = window(2, 0, &[&[0.1, 0.2, 0.3, 0.4], &[0.5, 0.6]]);
    assert_eq!(w.covered_until(2), 6);
    let mut out = [0.0; 4];
    render_stretched(&w, 2, 2, &mut out);
    assert_eq!(out, [0.3, 0.4, 0.5, 0.6]);
}

#[test]
fn sample_and_frame_conversions_round_trip() {
    let fps = 25.0;
    for frame in [0, 1, 24, 25, 1234] {
        let s = timeline_frame_to_sample(frame, fps, PROJECT_SAMPLE_RATE);
        assert_eq!(sample_to_timeline_frame(s, fps, PROJECT_SAMPLE_RATE), frame);
    }
}

fn downmix_interleaved(samples: &[f32], from: u16, to: u16) -> Vec<f32> {
    let mut out = Vec::new();
    remix_channels_into(samples, from, to, &mut out);
    out
}

#[test]
fn downmix_interleaved_is_a_noop_when_channel_counts_match() {
    let samples = vec![0.1, 0.2, 0.3, 0.4];
    assert_eq!(downmix_interleaved(&samples, 2, 2), samples);
}

#[test]
fn downmix_interleaved_averages_all_channels_to_mono() {
    // A stereo frame [1.0, 0.0] -> mono must give the average, 0.5.
    let samples = vec![1.0, 0.0, 0.5, 0.5];
    let mono = downmix_interleaved(&samples, 2, 1);
    assert_eq!(mono, vec![0.5, 0.5]);
}

#[test]
fn downmix_interleaved_six_to_two_groups_even_and_odd_channels() {
    // Typical ffmpeg order for 5.1(side): L,R,C,LFE,Ls,Rs. With
    // an even index -> channel 0 (L,C,Ls) and odd -> channel 1
    // (R,LFE,Rs): a frame with L=1.0 and all the others at 0 must end up
    // almost entirely on channel 0 (average of 1.0,0.0,0.0 = 1/3), nothing
    // on channel 1.
    let l_only = vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let stereo = downmix_interleaved(&l_only, 6, 2);
    assert_eq!(stereo.len(), 2);
    assert!((stereo[0] - (1.0 / 3.0)).abs() < 1e-6, "left={}", stereo[0]);
    assert_eq!(stereo[1], 0.0);
}

#[test]
fn downmix_interleaved_upmixes_mono_by_duplicating_to_every_channel() {
    let mono = vec![0.7, -0.3];
    let stereo = downmix_interleaved(&mono, 1, 2);
    assert_eq!(stereo, vec![0.7, 0.7, -0.3, -0.3]);
}

#[test]
fn downmix_interleaved_preserves_frame_count() {
    let samples = vec![0.0f32; 6 * 100]; // 100 frames at 6 channels
    let stereo = downmix_interleaved(&samples, 6, 2);
    assert_eq!(stereo.len(), 2 * 100);
}

#[test]
fn db_to_linear_matches_known_reference_points() {
    assert!((db_to_linear(0.0) - 1.0).abs() < 1e-6);
    // -6dB ~= halves the amplitude; +6dB ~= doubles it.
    assert!((db_to_linear(-6.0) - 0.5012).abs() < 1e-3);
    assert!((db_to_linear(6.0) - 1.9953).abs() < 1e-3);
    // -20dB = exactly a factor of 0.1.
    assert!((db_to_linear(-20.0) - 0.1).abs() < 1e-6);
}

fn fade_test_clip(fade_in: u64, fade_out: u64) -> MixClip {
    MixClip {
        clip: ClipId(0),
        start: 0,
        len: 1000,
        source_offset: 0,
        step: 1.0,
        input: ClipInput::Blocks(mono(Arc::new(Vec::new()))),
        gain_db: Keyframed::constant(0.0),
        track: 0,
        clip_fps: 10.0,
        media_offset: 0,
        media_step: 1.0,
        fade_in,
        fade_out,
    }
}

#[test]
fn fade_multiplier_ramps_linearly_in_and_out() {
    let clip = fade_test_clip(100, 200);
    assert_eq!(fade_multiplier(&clip, 0), 0.0);
    assert!((fade_multiplier(&clip, 50) - 0.5).abs() < 1e-6);
    assert_eq!(fade_multiplier(&clip, 100), 1.0);
    assert_eq!(
        fade_multiplier(&clip, 500),
        1.0,
        "on the plateau it stays at full volume"
    );
    assert!(
        (fade_multiplier(&clip, 900) - 0.5).abs() < 1e-6,
        "200 samples from the end"
    );
    assert_eq!(fade_multiplier(&clip, 1000), 0.0);
}

#[test]
fn no_fade_stays_at_full_volume() {
    let clip = fade_test_clip(0, 0);
    assert_eq!(fade_multiplier(&clip, 0), 1.0);
    assert_eq!(fade_multiplier(&clip, 500), 1.0);
    assert_eq!(fade_multiplier(&clip, 1000), 1.0);
}

#[test]
fn overlapping_fades_multiply_instead_of_dipping_below_either_ramp_alone() {
    // Fade in and out cover the whole clip: at the center each ramp is
    // 0.5, the product (not the minimum) is what one sees.
    let clip = fade_test_clip(1000, 1000);
    assert!((fade_multiplier(&clip, 500) - 0.25).abs() < 1e-6);
}

#[test]
fn a_fade_ramps_every_sample_even_inside_a_gain_block() {
    let (project, a, _) = project();
    let len = (GAIN_BLOCK_FRAMES * 3) as usize;
    let mut clip = clip_at(a, 0, 0, (len / 10) as FrameIdx);
    // 100 samples at each edge, an eighth of a gain block.
    clip.fade_in = 10;
    clip.fade_out = 10;
    let tl = timeline(vec![audio_track(vec![clip])]);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut |_: &Path, _: usize| {
        Some(mono(Arc::new(vec![0.5; len])))
    });
    let mut out = vec![9.0; len];
    mix_range(&snap, 0, &mut out);
    assert_eq!(out[0], 0.0, "the fade starts from silence");
    assert!((out[50] - 0.25).abs() < 1e-3, "halfway: {}", out[50]);
    assert!((out[99] - 0.495).abs() < 1e-3, "at its end: {}", out[99]);
    assert!(
        out[..100].windows(2).all(|pair| pair[1] >= pair[0]),
        "the ramp up is monotonic: {:?}",
        &out[..100]
    );
    assert!(
        out[100..len - 100].iter().all(|&s| s == 0.5),
        "between the fades the gain is unchanged"
    );
    assert!(
        out[len - 100..].windows(2).all(|pair| pair[1] <= pair[0]),
        "the ramp down is monotonic: {:?}",
        &out[len - 100..]
    );
    assert!(
        (out[len - 1] - 0.005).abs() < 1e-3,
        "last sample: {}",
        out[len - 1]
    );
}

/// A fade is a timeline property: a varispeed clip ramps over the same
/// timeline frames, reading its source faster underneath.
#[test]
fn a_varispeed_clip_fades_over_its_timeline_frames() {
    let (project, a, _) = project();
    let mut clip = sped_up_clip(a, Rational::new(2, 1), 240);
    clip.fade_in = 10;
    let tl = timeline(vec![audio_track(vec![clip])]);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut |_: &Path, _: usize| {
        Some(mono(Arc::new(vec![0.5; 10_000])))
    });
    let mut out = vec![9.0; 2400];
    mix_range(&snap, 0, &mut out);
    assert_eq!(out[0], 0.0, "the fade starts from silence");
    assert!((out[50] - 0.25).abs() < 1e-3, "halfway: {}", out[50]);
    assert!((out[99] - 0.495).abs() < 1e-3, "at its end: {}", out[99]);
    assert!(
        out[100..].iter().all(|&s| s == 0.5),
        "past the fade-in the gain is unchanged: {:?}",
        &out[95..105]
    );

    let mut state = MixState::new(&snap);
    let mut split = vec![9.0; 2400];
    mix_range_from(&snap, &mut state, 0, &mut split[..1234]);
    mix_range_from(&snap, &mut state, 1234, &mut split[1234..]);
    assert_eq!(split, out, "a split mix equals a whole one");
}

/// `b` from source frame 10 at `speed`, `len` timeline frames from 0,
/// varispeed.
fn sped_up_clip(media: vv_core::MediaId, speed: Rational, len: FrameIdx) -> Clip {
    let mut clip = clip_at(media, 0, 0, 1);
    clip.set_speed(speed, Rational::one());
    clip.pitch_correction = false;
    clip.source_offset = clip.rate().scale_round(10);
    clip.timeline_len = len;
    clip
}

#[test]
fn a_faster_clip_reads_its_audio_faster_averaging_what_it_skips() {
    let (project, _, b) = project();
    let tl = timeline(vec![audio_track(vec![sped_up_clip(
        b,
        Rational::new(2, 1),
        20,
    )])]);
    let out = render(&project, &tl, 0, 200);
    for i in [0usize, 7, 150] {
        let expected = (100.0 + 2.0 * i as f32 + 0.5) / 1000.0;
        assert!((out[i] - expected).abs() < 1e-5, "sample {i}: {}", out[i]);
    }
}

#[test]
fn a_slower_clip_interpolates_between_samples() {
    let (project, _, b) = project();
    let tl = timeline(vec![audio_track(vec![sped_up_clip(
        b,
        Rational::new(1, 2),
        40,
    )])]);
    let out = render(&project, &tl, 0, 400);
    for i in [0usize, 1, 301] {
        let expected = (100.0 + i as f32 / 2.0) / 1000.0;
        assert!((out[i] - expected).abs() < 1e-5, "sample {i}: {}", out[i]);
    }
}

/// Hands out a fixed "stretched" buffer and records what it was asked.
#[derive(Default)]
struct FakeStretch {
    ready: bool,
    asked: Vec<(std::ops::Range<u64>, f64)>,
    nested: bool,
    pitch: Vec<(u64, f64, bool)>,
}

impl AudioSource for FakeStretch {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        buffers(path, stream).map_or(ClipAudio::Missing, ClipAudio::Ready)
    }

    fn stretched(&mut self, request: StretchRequest) -> ClipAudio {
        self.pitch
            .push((request.key, request.pitch, request.preserve_formants));
        self.asked.push((request.frames.clone(), request.tempo));
        self.nested |= matches!(request.input, StretchInput::Nested { .. });
        if self.ready {
            ClipAudio::Ready(mono(Arc::new(vec![0.9; 100])))
        } else {
            ClipAudio::Pending
        }
    }
}

#[test]
fn pitch_edits_invalidate_the_processed_clip_and_bypass_uses_the_original() {
    let (project, media, _) = project();
    let mut tl = timeline(vec![audio_track(vec![clip_at(media, 0, 0, 10)])]);
    let mut source = FakeStretch::default();
    let mut ask = |tl: &Timeline| {
        MixSnapshot::from_timeline(&project, tl, RATE, 1, &mut source);
        source.pitch.last().copied()
    };
    assert_eq!(ask(&tl), None, "neutral clips do not request processing");
    tl.tracks[0].clips[0].effects.pitch_shift.semitones = 12.0;
    let first = ask(&tl).unwrap();
    assert_eq!((first.1, first.2), (2.0, false));
    tl.tracks[0].clips[0].effects.pitch_shift.cents = 50.0;
    let fine = ask(&tl).unwrap();
    assert_ne!(fine.0, first.0);
    tl.tracks[0].clips[0].effects.pitch_shift.preserve_formants = true;
    let formants = ask(&tl).unwrap();
    assert_ne!(formants.0, fine.0);
    assert!(formants.2);
    tl.tracks[0].clips[0].effects.pitch_shift.enabled = false;
    let mut source = FakeStretch::default();
    let snapshot = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut source);
    assert!(source.pitch.is_empty());
    let mut out = vec![0.0; 100];
    mix_range(&snapshot, 0, &mut out);
    assert!(out.iter().all(|s| *s == 0.5));
}

#[test]
fn varispeed_pitch_shift_keeps_the_pitch_ratio_within_the_processors_range() {
    let (project, media, _) = project();
    for speed in [Rational::new(1, 100), Rational::new(100, 1)] {
        let mut clip = sped_up_clip(media, speed, 1);
        clip.pitch_correction = false;
        clip.effects.pitch_shift.semitones = -24.0;
        let tl = timeline(vec![audio_track(vec![clip])]);
        let mut source = FakeStretch {
            ready: true,
            ..Default::default()
        };
        let snapshot = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut source);
        assert_eq!(source.asked[0].1, 1.0, "varispeed runs in the mixer");
        assert_eq!(source.pitch[0].1, 0.25);
        assert_eq!(snapshot.clips[0].step, speed.as_f64());
    }
}

/// Stretched from a render of its timeline, not from a mixdown of it.
#[test]
fn a_pitch_corrected_compound_clip_stretches_its_nested_timeline() {
    let (project, compound) = project_with_long_compound();
    let mut clip = sped_up_clip(compound, Rational::new(2, 1), 2);
    clip.pitch_correction = true;
    let tl = timeline(vec![audio_track(vec![clip])]);
    let mut source = FakeStretch {
        ready: true,
        ..FakeStretch::default()
    };
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut source);
    assert!(source.nested);
    assert_eq!(source.asked, [(100..140, 2.0)], "from its source offset");
    let mut out = vec![0.0; 20];
    mix_range(&snap, 0, &mut out);
    assert_eq!(out[0], 0.9);
}

/// Its audio ends at 600: past it the stretch would be silence.
#[test]
fn a_pitch_corrected_compound_clip_stretches_no_further_than_its_audio() {
    let (project, compound) = project_with_long_compound();
    let mut clip = sped_up_clip(compound, Rational::new(2, 1), 40);
    clip.pitch_correction = true;
    let tl = timeline(vec![audio_track(vec![clip])]);
    let mut source = FakeStretch::default();
    MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut source);
    assert_eq!(source.asked, [(100..600, 2.0)]);
}

#[test]
fn a_pitch_corrected_clip_plays_the_stretch_of_its_source_range_or_nothing() {
    let (project, _, b) = project();
    let mut clip = sped_up_clip(b, Rational::new(2, 1), 20);
    clip.pitch_correction = true;
    let tl = timeline(vec![audio_track(vec![clip])]);

    let mut source = FakeStretch {
        ready: true,
        ..FakeStretch::default()
    };
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut source);
    let mut out = vec![0.0; 200];
    mix_range(&snap, 0, &mut out);
    assert_eq!(source.asked, [(100..500, 2.0)]);
    assert_eq!(out[0], 0.9);
    assert_eq!(out[99], 0.9);
    assert_eq!(out[100], 0.0, "past the stretched audio");

    source.ready = false;
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut source);
    mix_range(&snap, 0, &mut out);
    assert!(out.iter().all(|&s| s == 0.0), "silent until it is ready");
}

#[test]
fn track_and_master_gains_scale_the_mix() {
    let (project, a, b) = project();
    let mut louder = audio_track(vec![clip_at(a, 0, 0, 5)]);
    louder.mix.gain_db = 20.0 * 2f32.log10();
    let mut tl = timeline(vec![louder, audio_track(vec![clip_at(b, 0, 0, 5)])]);
    tl.master.gain_db = -20.0 * 2f32.log10();
    let out = render(&project, &tl, 0, 5);
    for (i, s) in out.iter().enumerate() {
        let expected = (2.0 * 0.5 + i as f32 / 1000.0) / 2.0;
        assert!((s - expected).abs() < 1e-5, "{s} != {expected}");
    }
}

#[test]
fn a_compound_clip_plays_through_its_nested_master() {
    let (mut project, compound) = project_with_compound();
    let nested = project.media_pool[compound].compound.unwrap();
    project.timelines[nested].master.gain_db = -20.0 * 2f32.log10();
    let tl = timeline(vec![audio_track(vec![clip_at(compound, 0, 0, 5)])]);
    let out = render(&project, &tl, 0, 50);
    assert!(out.iter().all(|&s| (s - 0.25).abs() < 1e-5));
}

#[test]
fn balance_attenuates_only_the_opposite_channel() {
    assert_eq!(balance_gains(0.0), [1.0, 1.0]);
    assert_eq!(balance_gains(-0.25), [1.0, 0.75]);
    assert_eq!(balance_gains(2.0), [0.0, 1.0], "clamped");

    let (project, a, _) = project();
    let mut left = audio_track(vec![clip_at(a, 0, 0, 5)]);
    left.mix.pan = -0.5;
    let mut tl = timeline(vec![left]);
    tl.master.pan = 0.5;
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 2, &mut stereo);
    let mut out = vec![9.0; 4];
    mix_range(&snap, 0, &mut out);
    assert_eq!(out, [0.25, 0.25, 0.25, 0.25]);
}

#[test]
fn only_the_metered_mix_raises_the_meters_and_taking_resets_them() {
    let (project, a, b) = project();
    let mut quiet = audio_track(vec![clip_at(a, 0, 0, 5)]);
    quiet.mix.gain_db = -20.0 * 2f32.log10();
    let tl = timeline(vec![quiet, audio_track(vec![clip_at(b, 0, 0, 5)])]);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
    let mut out = vec![0.0; 20];

    mix_range(&snap, 0, &mut out);
    assert_eq!(snap.meters.master().take(), (0.0, 0.0));

    mix_range_metered(&snap, &mut MixState::new(&snap), 0, &mut out);
    let (l, r) = snap.meters.track(0).unwrap().take();
    assert!((l - 0.25).abs() < 1e-5 && l == r, "mono on both sides");
    assert_eq!(snap.meters.track(1).unwrap().take().0, 19.0 / 1000.0);
    assert!((snap.meters.master().take().0 - (0.25 + 0.019)).abs() < 1e-5);
    assert_eq!(snap.meters.track(0).unwrap().take(), (0.0, 0.0));
}

fn normalize(target_db: f32) -> vv_core::AudioEffect {
    normalize_with(target_db, NormalizeMode::SamplePeak, SetLevel::Relative)
}

fn normalize_with(
    target_db: f32,
    mode: NormalizeMode,
    set_level: SetLevel,
) -> vv_core::AudioEffect {
    vv_core::AudioEffect::new(vv_core::AudioEffectKind::Normalize {
        target_db,
        mode,
        set_level,
    })
}

#[test]
fn independent_normalization_brings_every_clip_to_the_target() {
    let (project, _, b) = project();
    // Peaks 0.049 and 0.549, a gap between them.
    let clips = vec![clip_at(b, 0, 0, 5), clip_at(b, 10, 50, 5)];
    let mut track = audio_track(clips);
    track.mix.effects = vec![normalize_with(
        0.0,
        NormalizeMode::SamplePeak,
        SetLevel::Independent,
    )];
    let out = render(&project, &timeline(vec![track]), 0, 150);
    let peak =
        |range: std::ops::Range<usize>| out[range].iter().fold(0.0f32, |p, s| p.max(s.abs()));
    assert!((peak(0..50) - 1.0).abs() < 1e-5, "{}", peak(0..50));
    assert!((peak(100..150) - 1.0).abs() < 1e-5, "{}", peak(100..150));
}

/// Reads the level of each clip from where it starts.
struct LevelByStart(Vec<(u64, f32)>);

impl AudioSource for LevelByStart {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        buffers(path, stream).map_or(ClipAudio::Missing, ClipAudio::Ready)
    }

    fn level(&mut self, analysis: LevelAnalysis) -> LevelReading {
        let start = analysis.segments[0].frames.start;
        let level = self.0.iter().find(|(s, _)| *s == start).map(|(_, l)| *l);
        LevelReading::Ready(level.unwrap())
    }
}

#[test]
fn set_level_relative_keeps_the_differences_and_brings_the_loudest_clip_to_the_target() {
    let (project, a, _) = project();
    let mut track = audio_track(vec![clip_at(a, 0, 0, 5), clip_at(a, 10, 0, 5)]);
    let target = 20.0 * 0.4f32.log10();
    let mut mix = |set_level| {
        track.mix.effects = vec![normalize_with(target, NormalizeMode::Loudness, set_level)];
        let tl = timeline(vec![track.clone()]);
        let mut source = LevelByStart(vec![(0, 0.1), (100, 0.2)]);
        let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut source);
        let mut out = vec![9.0; 150];
        mix_range(&snap, 0, &mut out);
        (out[25], out[125])
    };
    let (first, second) = mix(SetLevel::Relative);
    assert!((first - 1.0).abs() < 1e-5 && (second - 1.0).abs() < 1e-5);
    let (first, second) = mix(SetLevel::Independent);
    assert!((first - 2.0).abs() < 1e-5 && (second - 1.0).abs() < 1e-5);
}

#[test]
fn track_normalization_brings_its_peak_to_the_target_before_the_fader() {
    let (project, a, _) = project();
    let mut track = audio_track(vec![clip_at(a, 0, 0, 5)]);
    track.mix.effects = vec![normalize(-12.0), normalize(0.0)];
    track.mix.gain_db = -20.0 * 2f32.log10();
    let tl = timeline(vec![track]);
    let out = render(&project, &tl, 0, 50);
    assert!(
        out.iter().all(|&s| (s - 0.5).abs() < 1e-5),
        "last one wins, then the fader"
    );
}

#[test]
fn a_disabled_normalization_does_nothing_and_silence_stays_silent() {
    let (project, a, _) = project();
    let mut track = audio_track(vec![clip_at(a, 0, 0, 5)]);
    track.mix.effects = vec![vv_core::AudioEffect {
        enabled: false,
        ..normalize(0.0)
    }];
    let mut empty = audio_track(Vec::new());
    empty.mix.effects = vec![normalize(0.0)];
    let tl = timeline(vec![track, empty]);
    assert!(render(&project, &tl, 0, 50).iter().all(|&s| s == 0.5));
}

#[test]
fn master_normalization_measures_the_mix_after_the_track_faders() {
    let (project, a, _) = project();
    let mut quiet = audio_track(vec![clip_at(a, 0, 0, 5)]);
    quiet.mix.gain_db = -20.0 * 4f32.log10();
    let mut tl = timeline(vec![quiet, audio_track(vec![clip_at(a, 0, 0, 5)])]);
    tl.master.effects = vec![normalize(-20.0 * 2f32.log10())];
    // 0.125 + 0.5 = 0.625, brought to 0.5.
    let out = render(&project, &tl, 0, 50);
    assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-5));
}

/// Measures nothing: every peak is on its way, with `last` as the reading
/// kept from before.
struct PendingPeaks {
    last: Option<f32>,
}

impl AudioSource for PendingPeaks {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        buffers(path, stream).map_or(ClipAudio::Missing, ClipAudio::Ready)
    }

    fn level(&mut self, _: LevelAnalysis) -> LevelReading {
        LevelReading::Pending(self.last)
    }
}

#[test]
fn a_pending_peak_plays_the_last_reading() {
    let (mut project, compound) = project_with_compound();
    let nested = project.media_pool[compound].compound.unwrap();
    project.timelines[nested].tracks[0].mix.effects = vec![normalize(0.0)];
    let tl = timeline(vec![audio_track(vec![clip_at(compound, 0, 0, 5)])]);

    let mut source = PendingPeaks { last: Some(0.25) };
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut source);
    let mut out = vec![0.0; 50];
    mix_range(&snap, 0, &mut out);
    assert!(out.iter().all(|&s| (s - 2.0).abs() < 1e-5), "0.5 * 1/0.25");

    source.last = None;
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut source);
    mix_range(&snap, 0, &mut out);
    assert!(out.iter().all(|&s| s == 0.5), "unity until measured");
}

fn compressor() -> vv_core::AudioEffect {
    let mut params = vv_core::MultibandCompressor::DEFAULT;
    for band in &mut params.bands {
        band.threshold_db = -30.0;
        band.ratio = 8.0;
    }
    vv_core::AudioEffect::new(vv_core::AudioEffectKind::MultibandCompressor(params))
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |p, s| p.max(s.abs()))
}

#[test]
fn a_normalization_after_a_compressor_measures_the_compressed_signal() {
    let (project, _, b) = project();
    let mut track = audio_track(vec![clip_at(b, 0, 0, 90)]);
    track.mix.effects = vec![compressor(), normalize(-20.0 * 2f32.log10())];
    let tl = timeline(vec![track.clone()]);
    let out = render(&project, &tl, 0, 900);
    assert!((peak(&out) - 0.5).abs() < 1e-3, "{}", peak(&out));

    track.mix.effects.reverse();
    let tl = timeline(vec![track]);
    assert!(
        peak(&render(&project, &tl, 0, 900)) < 0.4,
        "compressed after"
    );
}

#[test]
fn the_output_stream_carries_the_effects_over_from_one_call_to_the_next() {
    let (project, _, b) = project();
    let mut track = audio_track(vec![clip_at(b, 0, 0, 90)]);
    track.mix.effects = vec![compressor()];
    let tl = timeline(vec![track]);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
    let whole = render(&project, &tl, 0, 600);

    let mut state = MixState::new(&snap);
    let mut halves = vec![0.0; 600];
    let (first, second) = halves.split_at_mut(300);
    mix_range_metered(&snap, &mut state, 0, first);
    mix_range_metered(&snap, &mut state, 300, second);
    assert_eq!(halves, whole);

    let mut jumped = vec![0.0; 300];
    mix_range_metered(&snap, &mut state, 300, &mut jumped);
    assert_eq!(
        jumped,
        render(&project, &tl, 300, 300),
        "a seek starts over"
    );
}

#[test]
fn the_compressor_reports_its_reduction_at_its_place_in_the_strip() {
    let (project, _, b) = project();
    let mut track = audio_track(vec![clip_at(b, 0, 0, 90)]);
    let disabled = vv_core::AudioEffect {
        enabled: false,
        ..normalize(0.0)
    };
    track.mix.effects = vec![disabled, compressor()];
    let tl = timeline(vec![track]);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
    let meter = || snap.meters.band_meter(MixerChannel::Track(0), 1).unwrap();
    let mut out = vec![0.0; 900];
    let silent = crate::dynamics::BandActivity::default();

    mix_range(&snap, 0, &mut out);
    assert_eq!(meter().take(), silent);

    mix_range_metered(&snap, &mut MixState::new(&snap), 0, &mut out);
    let activity = meter().take();
    assert!(
        activity.reduction_db.iter().any(|r| *r > 1.0),
        "{activity:?}"
    );
    assert!(activity.level.iter().any(|l| *l > 0.01), "{activity:?}");
    assert_eq!(meter().take(), silent);
    let disabled = snap.meters.band_meter(MixerChannel::Track(0), 0).unwrap();
    assert_eq!(disabled.take(), silent);
}

#[test]
fn the_output_stream_feeds_the_compressor_spectrum_and_a_new_snapshot_keeps_it() {
    let (project, _, b) = project();
    let mut track = audio_track(vec![clip_at(b, 0, 0, 90)]);
    track.mix.effects = vec![compressor()];
    let tl = timeline(vec![track]);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
    let tap = || snap.meters.spectrum(MixerChannel::Track(0), 0).unwrap();
    let mut out = vec![0.0; 900];

    mix_range(&snap, 0, &mut out);
    assert_eq!(tap().written(), 0);
    mix_range_metered(&snap, &mut MixState::new(&snap), 0, &mut out);
    assert_eq!(tap().written(), 900);

    let mut next = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
    next.meters.keep_spectra_of(&snap.meters);
    let kept = next.meters.spectrum(MixerChannel::Track(0), 0).unwrap();
    assert_eq!(kept.written(), 900);
    assert!(snap.meters.spectrum(MixerChannel::Track(0), 1).is_none());
}

#[test]
fn mono_sends_the_average_of_the_channels_to_all_of_them() {
    let (project, a, _) = project();
    let mut track = audio_track(vec![clip_at(a, 0, 0, 5)]);
    track.mix.effects = vec![vv_core::AudioEffect::new(vv_core::AudioEffectKind::Mono)];
    // The balance comes after the chain: hard left, the right side stays
    // silent even after the mono.
    track.mix.pan = -1.0;
    let tl = timeline(vec![track]);
    // Sound on the left channel only.
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 2, &mut |path: &Path, _| {
        (path.to_str()? == "a.wav")
            .then(|| SampleBlocks::contiguous(Arc::new([0.8f32, 0.0].repeat(500)), 0, 2))
    });
    let mut out = vec![0.0; 20];
    mix_range(&snap, 0, &mut out);
    assert_eq!(&out[..4], [0.4, 0.0, 0.4, 0.0]);

    let mut centred = tl.clone();
    centred.tracks[0].mix.pan = 0.0;
    let snap = MixSnapshot::from_timeline(&project, &centred, RATE, 2, &mut |path: &Path, _| {
        (path.to_str()? == "a.wav")
            .then(|| SampleBlocks::contiguous(Arc::new([0.8f32, 0.0].repeat(500)), 0, 2))
    });
    mix_range(&snap, 0, &mut out);
    assert_eq!(&out[..4], [0.4, 0.4, 0.4, 0.4]);
}

#[test]
fn an_equalizer_in_the_chain_shapes_the_audio_and_feeds_its_spectrum() {
    let (project, _, b) = project();
    let mut track = audio_track(vec![clip_at(b, 0, 0, 90)]);
    let mut params = vv_core::Equalizer::DEFAULT;
    // The clip is a slow ramp: all below the shelf.
    params.bands[1].freq_hz = 2000.0;
    params.bands[1].gain_db = -12.0;
    track.mix.effects = vec![vv_core::AudioEffect::new(
        vv_core::AudioEffectKind::Equalizer(params),
    )];
    let tl = timeline(vec![track]);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
    let mut out = vec![0.0; 900];
    mix_range_metered(&snap, &mut MixState::new(&snap), 0, &mut out);
    let (shaped, ramp) = (out[800], 0.8);
    assert!((shaped / ramp - 0.25).abs() < 0.02, "{shaped}");
    let tap = snap.meters.spectrum(MixerChannel::Track(0), 0).unwrap();
    assert_eq!(tap.written(), 900);
}

/// Hands out only the frames asked for, and records them.
#[derive(Default)]
struct Windowed {
    asked: Vec<(PathBuf, std::ops::Range<u64>)>,
}

impl AudioSource for Windowed {
    fn file(&mut self, _path: &Path, _stream: usize) -> ClipAudio {
        unreachable!("a range mix asks for frames")
    }

    fn file_frames(
        &mut self,
        path: &Path,
        stream: usize,
        frames: std::ops::Range<u64>,
        _plays_at: u64,
    ) -> ClipAudio {
        self.asked.push((path.to_path_buf(), frames.clone()));
        let Some(whole) = samples(path, stream) else {
            return ClipAudio::Missing;
        };
        let end = (frames.end as usize).min(whole.len());
        let start = (frames.start as usize).min(end);
        let window = Arc::new(whole[start..end].to_vec());
        ClipAudio::Ready(SampleBlocks::contiguous(window, start as u64, 1))
    }
}

#[test]
fn a_range_mix_reads_only_its_frames_and_matches_the_whole_mix() {
    let (project, a, b) = project();
    let mut offset = clip_at(b, 2, 3, 20);
    offset.fade_out = 4;
    let mut faster = sped_up_clip(b, Rational::new(3, 2), 30);
    faster.timeline_start = 5;
    // `b` ends 10 frames into it: the fade-out comes before its end.
    let mut past_the_file = clip_at(b, 10, 90, 20);
    past_the_file.fade_out = 5;
    let tl = timeline(vec![
        audio_track(vec![offset, clip_at(a, 80, 0, 5)]),
        audio_track(vec![faster]),
        audio_track(vec![past_the_file]),
    ]);

    let mut source = Windowed::default();
    let snap = MixSnapshot::from_timeline_range(&project, &tl, RATE, 1, &mut source, 100..250);
    let mut out = vec![9.0; 150];
    mix_range(&snap, 100, &mut out);

    assert_eq!(out, render(&project, &tl, 100, 150));
    assert!(
        source
            .asked
            .iter()
            .all(|(path, frames)| path.ends_with("b.wav") && frames.start > 0),
        "{:?}",
        source.asked
    );
}

#[test]
fn a_range_mix_of_a_normalizing_timeline_reads_only_its_frames() {
    let (project, _, b) = project();
    let mut tl = timeline(vec![audio_track(vec![clip_at(b, 0, 0, 50)])]);
    tl.master.effects.push(vv_core::AudioEffect {
        enabled: true,
        kind: AudioEffectKind::Normalize {
            target_db: -1.0,
            mode: NormalizeMode::SamplePeak,
            set_level: SetLevel::Relative,
        },
    });
    let mut source = Windowed::default();
    let snap = MixSnapshot::from_timeline_range(&project, &tl, RATE, 1, &mut source, 100..150);
    let mut out = vec![9.0; 50];
    mix_range(&snap, 100, &mut out);

    assert_eq!(out, render(&project, &tl, 100, 50));
    let ClipInput::Blocks(buffer) = &snap.clips[0].input else {
        panic!("a file clip");
    };
    assert!(buffer.start() >= 100, "its level is measured apart");
}

/// `Windowed`, its levels measured elsewhere: what it is asked for is what
/// the mix reads.
#[derive(Default)]
struct MeasuredApart(Windowed);

impl AudioSource for MeasuredApart {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        self.0.file(path, stream)
    }

    fn file_frames(
        &mut self,
        path: &Path,
        stream: usize,
        frames: std::ops::Range<u64>,
        plays_at: u64,
    ) -> ClipAudio {
        self.0.file_frames(path, stream, frames, plays_at)
    }

    fn level(&mut self, _: LevelAnalysis) -> LevelReading {
        LevelReading::Ready(0.5)
    }
}

/// Audio still decoding, counting the measures asked for.
#[derive(Default)]
struct PartialLevels {
    measured: usize,
}

impl AudioSource for PartialLevels {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        buffers(path, stream).map_or(ClipAudio::Missing, ClipAudio::Partial)
    }

    fn level(&mut self, _: LevelAnalysis) -> LevelReading {
        self.measured += 1;
        LevelReading::Ready(0.25)
    }
}

/// The level is measured from the files, not from what is decoded yet.
#[test]
fn a_normalization_is_measured_while_its_audio_decodes() {
    let (project, a, _) = project();
    let mut track = audio_track(vec![clip_at(a, 0, 0, 5)]);
    track.mix.effects = vec![normalize(0.0)];
    let tl = timeline(vec![track]);
    let mut source = PartialLevels::default();
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut source);
    assert_eq!(source.measured, 1);
    let mut out = vec![9.0; 50];
    mix_range(&snap, 0, &mut out);
    assert!(out.iter().all(|&s| s == 2.0), "0.5 at four times 0.25");
}

/// `project()` plus a compound clip whose nested timeline plays the ramp
/// of `b.wav` for 60 frames through a compressor, over `a.wav` on another
/// track.
fn project_with_long_compound() -> (Project, vv_core::MediaId) {
    let (mut project, a, b) = project();
    let mut ramp = audio_track(vec![clip_at(b, 0, 10, 60)]);
    ramp.mix.effects = vec![compressor()];
    let nested = project.timelines.insert(timeline(vec![
        ramp,
        audio_track(vec![clip_at(a, 20, 0, 10)]),
    ]));
    let compound = project.media_pool.insert(MediaItem {
        path: PathBuf::from("Compound Clip 1"),
        meta: project.media_pool[a].meta.clone(),
        content_hash: 10,
        compound: Some(nested),
        folder: None,
    });
    (project, compound)
}

/// What a compound clip played before being mixed live: its whole mixdown,
/// read like a file.
fn with_mixdowns(project: &Project, snap: &MixSnapshot, compound: MediaId) -> MixSnapshot {
    let nested = &project.timelines[project.media_pool[compound].compound.unwrap()];
    let nested = MixSnapshot::from_timeline(project, nested, RATE, 1, &mut buffers);
    let len = nested.clips.iter().map(|c| c.start + c.len).max().unwrap();
    let mut samples = vec![0.0; len as usize];
    mix_range(&nested, 0, &mut samples);
    let mixdown = mono(Arc::new(samples));
    let clips = snap
        .clips
        .iter()
        .map(|clip| MixClip {
            input: ClipInput::Blocks(mixdown.clone()),
            ..clip.clone()
        })
        .collect();
    MixSnapshot::new(RATE, 1, clips, snap.tracks.clone(), snap.master.clone())
}

#[test]
fn a_compound_mixed_live_plays_as_its_mixdown() {
    let (project, compound) = project_with_long_compound();
    let mut gain = clip_at(compound, 2, 0, 30);
    gain.fade_out = 5;
    let mut keyframes = Keyframed::constant(-3.0f32);
    keyframes.upsert(0, 0.0, Interpolation::Linear);
    keyframes.upsert(20, -12.0, Interpolation::Linear);
    gain.effects.gain_db = keyframes;
    let mut faster = Clip::new(
        ClipId(1),
        ClipSource::Media(compound),
        0,
        40,
        20,
        Rational::one(),
        Rational::new(3, 2),
    );
    let mut slower = Clip::new(
        ClipId(2),
        ClipSource::Media(compound),
        0,
        70,
        30,
        Rational::one(),
        Rational::new(2, 3),
    );
    faster.pitch_correction = false;
    slower.pitch_correction = false;
    let tracks = [vec![gain], vec![faster], vec![slower]];
    for clips in tracks {
        let tl = timeline(vec![audio_track(clips)]);
        let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
        let mut live = vec![9.0; 1000];
        mix_range(&snap, 0, &mut live);
        let mut mixdown = vec![9.0; 1000];
        mix_range(&with_mixdowns(&project, &snap, compound), 0, &mut mixdown);
        assert_eq!(live, mixdown);
        assert!(live.iter().any(|&s| s != 0.0));
    }
}

/// As a file clip past its file's end: the fade-out is on the audio, not
/// on the silence after it.
#[test]
fn a_compound_longer_than_its_audio_fades_out_where_its_audio_ends() {
    let (project, compound) = project_with_compound();
    let mut clip = clip_at(compound, 0, 0, 10);
    clip.fade_out = 2;
    let tl = timeline(vec![audio_track(vec![clip])]);
    // Checked on the clip: at `RATE` a gain block outlasts the fade.
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
    assert_eq!((snap.clips[0].len, snap.clips[0].fade_out), (50, 20));
}

/// The nested compressor runs in order across calls, and a varispeed read
/// does not render twice the frame two calls share.
#[test]
fn a_compound_mixed_in_many_calls_equals_one_call() {
    let (project, compound) = project_with_long_compound();
    for speed in [Rational::one(), Rational::new(3, 2), Rational::new(2, 3)] {
        let mut clip = Clip::new(
            ClipId(1),
            ClipSource::Media(compound),
            0,
            5,
            30,
            Rational::one(),
            speed,
        );
        clip.pitch_correction = false;
        let tl = timeline(vec![audio_track(vec![clip])]);
        let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
        let mut whole = vec![0.0; 600];
        mix_range(&snap, 0, &mut whole);
        let mut state = MixState::new(&snap);
        let mut split = Vec::new();
        for (start, len) in [(0, 7), (7, 64), (71, 1), (72, 300), (372, 228)] {
            let mut out = vec![0.0; len];
            mix_range_from(&snap, &mut state, start as u64, &mut out);
            split.extend(out);
        }
        assert_eq!(split, whole, "at speed {speed:?}");
    }
}

/// An edit elsewhere publishes a new snapshot: the compound's compressor
/// must carry on, not restart.
#[test]
fn a_new_snapshot_carries_on_a_compounds_effects() {
    let (project, compound) = project_with_long_compound();
    let tl = timeline(vec![audio_track(vec![clip_at(compound, 0, 0, 60)])]);
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
    let mut whole = vec![0.0; 600];
    mix_range(&snap, 0, &mut whole);

    let mut state = MixState::new(&snap);
    let mut first = vec![0.0; 250];
    mix_range_from(&snap, &mut state, 0, &mut first);
    let next = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut buffers);
    let mut next_state = MixState::new(&next);
    next_state.continue_from(&state);
    let mut second = vec![0.0; 350];
    mix_range_from(&next, &mut next_state, 250, &mut second);
    first.extend(second);
    assert_eq!(first, whole);
}

/// A track inserted above it moves the compound to another track index:
/// its compressor carries on all the same.
#[test]
fn a_compound_moved_to_another_track_carries_on_its_effects() {
    let (project, compound) = project_with_long_compound();
    let clip = clip_at(compound, 0, 0, 60);
    let before = timeline(vec![audio_track(vec![clip.clone()])]);
    let after = timeline(vec![audio_track(Vec::new()), audio_track(vec![clip])]);
    let next = MixSnapshot::from_timeline(&project, &after, RATE, 1, &mut buffers);
    let mut whole = vec![0.0; 600];
    mix_range(&next, 0, &mut whole);

    let snap = MixSnapshot::from_timeline(&project, &before, RATE, 1, &mut buffers);
    let mut state = MixState::new(&snap);
    let mut first = vec![0.0; 250];
    mix_range_from(&snap, &mut state, 0, &mut first);
    let mut next_state = MixState::new(&next);
    next_state.continue_from(&state);
    let mut second = vec![0.0; 350];
    mix_range_from(&next, &mut next_state, 250, &mut second);
    first.extend(second);
    assert_eq!(first, whole);
}

#[test]
fn a_compound_whose_timeline_normalizes_reads_only_what_plays() {
    let (mut project, compound) = project_with_long_compound();
    let nested = project.media_pool[compound].compound.unwrap();
    project.timelines[nested].master.effects = vec![normalize(0.0)];
    let tl = timeline(vec![audio_track(vec![clip_at(compound, 0, 0, 60)])]);
    let mut source = MeasuredApart::default();
    MixSnapshot::from_timeline_range(&project, &tl, RATE, 1, &mut source, 300..310);
    let ramp = source
        .0
        .asked
        .iter()
        .find(|(path, _)| path.as_os_str() == "b.wav");
    let frames = &ramp.unwrap().1;
    assert!(frames.end - frames.start < 50, "{frames:?}");
}

/// What the fast forward asks before stretching a window: a render of it now
/// equals the one once everything is decoded.
#[test]
fn a_snapshot_is_ready_where_its_clips_have_their_audio() {
    let (mut project, a, b) = project();
    let nested = project
        .timelines
        .insert(timeline(vec![audio_track(vec![clip_at(b, 0, 0, 10)])]));
    let compound = project.media_pool.insert(MediaItem {
        path: PathBuf::from("Compound Clip 1"),
        meta: project.media_pool[a].meta.clone(),
        content_hash: 10,
        compound: Some(nested),
        folder: None,
    });
    let tl = timeline(vec![
        audio_track(vec![clip_at(a, 0, 0, 10)]),
        audio_track(vec![clip_at(b, 20, 0, 10), clip_at(compound, 40, 0, 10)]),
    ]);
    // `b.wav` still decoding, no block yet.
    let mut source = |path: &Path, stream: usize| match path.to_str() {
        Some("b.wav") => Some(SampleBlocks::new(0, 1000, 1, vec![None], 1000)),
        _ => buffers(path, stream),
    };
    let snap = MixSnapshot::from_timeline(&project, &tl, RATE, 1, &mut source);
    assert!(snap.is_ready(0..100));
    assert!(!snap.is_ready(150..250), "b.wav");
    assert!(!snap.is_ready(400..450), "b.wav inside the compound clip");
    assert!(snap.is_ready(1000..2000), "nothing plays there");
}
