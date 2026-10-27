use super::*;
use vv_audio::MixSnapshot;
use vv_core::{MediaId, Project};

fn wait_done(cache: &mut MixBufferCache) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while cache.has_pending() {
        assert!(
            std::time::Instant::now() < deadline,
            "decode never finished"
        );
        std::thread::yield_now();
    }
}

/// A project whose compound clip plays the first `secs` of `path` (25 fps),
/// and a timeline playing the compound from its start.
fn compound_project(path: &Path, secs: u32) -> (Project, MediaId, vv_core::Timeline) {
    let mut project = Project::default();
    let real_media = project.media_pool.insert(vv_core::MediaItem {
        path: path.to_path_buf(),
        meta: vv_core::MediaMeta {
            duration_frames: 25 * secs as i64,
            fps: vv_core::Rational::new(25, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 48_000,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let real_clip = vv_core::Clip::from_source_range(
        vv_core::ClipId(1),
        vv_core::ClipSource::Media(real_media),
        0,
        25 * secs as i64,
        0,
        vv_core::Rational::one(),
    );
    let nested_id = project.timelines.insert(vv_core::Timeline {
        name: "Nested".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1, 1),
        tracks: vec![vv_core::Track {
            kind: vv_core::TrackKind::Audio,
            clips: vec![real_clip],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        }],
        markers: Vec::new(),
        master: Default::default(),
    });
    let compound_media = project.media_pool.insert(vv_core::MediaItem {
        path: "Compound Clip 1".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 25 * secs as i64,
            fps: vv_core::Rational::new(25, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 48_000,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 2,
        compound: Some(nested_id),
        folder: None,
    });

    let compound_clip = vv_core::Clip::from_source_range(
        vv_core::ClipId(2),
        vv_core::ClipSource::Media(compound_media),
        0,
        25 * secs as i64,
        0,
        vv_core::Rational::one(),
    );
    let mut parent = project.timelines[nested_id].clone();
    parent.tracks[0].clips = vec![compound_clip];
    (project, compound_media, parent)
}

/// Builds a file clip whose pitch-corrected duration is half its source duration.
fn pitch_corrected_file_project(path: &Path, source_secs: u32) -> (Project, vv_core::Timeline) {
    let mut project = Project::default();
    let media = project.media_pool.insert(vv_core::MediaItem {
        path: path.to_path_buf(),
        meta: vv_core::MediaMeta {
            duration_frames: 25 * source_secs as i64,
            fps: vv_core::Rational::new(25, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 48_000,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let mut clip = vv_core::Clip::new(
        vv_core::ClipId(1),
        vv_core::ClipSource::Media(media),
        0,
        0,
        25 * source_secs as i64 / 2,
        vv_core::Rational::one(),
        vv_core::Rational::new(2, 1),
    );
    clip.pitch_correction = true;
    let timeline = vv_core::Timeline {
        name: "sliding fallback".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1, 1),
        tracks: vec![vv_core::Track {
            kind: vv_core::TrackKind::Audio,
            clips: vec![clip],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        }],
        markers: Vec::new(),
        master: Default::default(),
    };
    (project, timeline)
}

fn sine_wav(name: &str, secs: u32) -> PathBuf {
    let dir = std::env::temp_dir().join("vv-app-mix-buffers-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=440:sample_rate=48000:duration={secs}"),
        ],
        &path,
    );
    path
}

const SEC: u64 = 48_000;

/// Frames of stream 0 that play at their own position.
fn read(cache: &mut MixBufferCache, path: &Path, frames: Range<u64>) -> ClipAudio {
    let plays_at = frames.start;
    cache.file_frames(path, 0, frames, plays_at)
}

fn ready(cache: &mut MixBufferCache, path: &Path, frames: Range<u64>) -> SampleBlocks {
    match read(cache, path, frames) {
        ClipAudio::Ready(buffer) => buffer,
        _ => panic!("expected every block decoded"),
    }
}

fn blocks_held(cache: &MixBufferCache, path: &Path) -> Vec<u64> {
    let mut held: Vec<u64> = cache
        .streams
        .get(&(path.to_path_buf(), 0))
        .map(|stream| stream.blocks.keys().copied().collect())
        .unwrap_or_default();
    held.sort_unstable();
    held
}

#[test]
fn buffers_are_decoded_in_background_per_path_and_stream() {
    let dir = std::env::temp_dir().join("vv-app-mix-buffers-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("two_streams.mkv");
    // Stream 0 mono 44.1 kHz of 1s, stream 1 stereo 48 kHz of 0.5s.
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=48000:duration=0.5",
            "-map",
            "0:a",
            "-map",
            "1:a",
            "-ac:1",
            "2",
            "-c:a",
            "pcm_f32le",
        ],
        &path,
    );

    let mut cache = MixBufferCache::spawn(48_000, 2);
    let whole = 0..2 * SEC;
    for stream in [0, 1] {
        let decoding = cache.media_frames(&path, stream, whole.clone()).unwrap();
        assert!(!decoding.holds(whole.clone()), "nothing decoded yet");
    }
    wait_done(&mut cache);

    let s0 = cache
        .media_frames(&path, 0, whole.clone())
        .expect("stream 0 ready");
    let s1 = cache
        .media_frames(&path, 1, whole.clone())
        .expect("stream 1 ready");
    assert!((s0.end() as i64 - 48_000).abs() < 100, "s0={}", s0.end());
    assert!((s1.end() as i64 - 24_000).abs() < 100, "s1={}", s1.end());
    cache.media_frames(&path, 5, whole.clone());
    wait_done(&mut cache);
    assert!(
        matches!(cache.file_frames(&path, 5, whole, 0), ClipAudio::Missing),
        "nonexistent stream"
    );
}

#[test]
fn a_long_read_plays_its_first_blocks_before_decoding_ends() {
    let path = sine_wav("long.wav", 20);
    let mut cache = MixBufferCache::spawn(48_000, 2);
    read(&mut cache, &path, 0..20 * SEC);
    let mut partial = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while cache.has_pending() {
        assert!(
            std::time::Instant::now() < deadline,
            "decode never finished"
        );
        partial |= matches!(read(&mut cache, &path, 0..20 * SEC), ClipAudio::Partial(_));
        std::thread::yield_now();
    }
    let full = ready(&mut cache, &path, 0..20 * SEC);
    assert_eq!(full.end(), 20 * SEC);
    assert!(partial, "expected blocks to play before the end");
}

#[test]
fn the_job_closest_to_the_playhead_is_decoded_first() {
    let path = sine_wav("closest.wav", 60);
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_playhead(40 * SEC);
    cache.begin_pass();
    read(&mut cache, &path, 0..60 * SEC);
    cache.sweep_unused();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let first = loop {
        assert!(std::time::Instant::now() < deadline, "no block arrived");
        cache.poll();
        let held = blocks_held(&cache, &path);
        if !held.is_empty() {
            break held;
        }
        std::thread::yield_now();
    };
    let playhead_block = 40 * SEC / BLOCK_FRAMES;
    assert!(
        first
            .iter()
            .all(|&index| index.abs_diff(playhead_block) <= JOB_BLOCKS),
        "first blocks {first:?}, playhead in block {playhead_block}"
    );
}

#[test]
fn over_budget_the_blocks_closest_to_the_playhead_are_kept() {
    let path = sine_wav("budget.wav", 60);
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(4 * BLOCK_FRAMES as usize * 4);
    let pass = |cache: &mut MixBufferCache| {
        cache.begin_pass();
        read(cache, &path, 0..60 * SEC);
        cache.sweep_unused();
        wait_done(cache);
    };
    cache.set_playhead(30 * SEC);
    pass(&mut cache);
    assert_eq!(blocks_held(&cache, &path), [20, 21, 22, 23]);

    cache.set_playhead(50 * SEC);
    wait_done(&mut cache);
    assert_eq!(blocks_held(&cache, &path), [35, 36, 37, 38]);
}

const MONO_BLOCK_BYTES: usize = BLOCK_FRAMES as usize * std::mem::size_of::<f32>();

/// Totals the sample-vector capacity resident in both audio caches.
fn resident_audio_bytes(cache: &MixBufferCache) -> usize {
    let block_bytes = cache
        .streams
        .values()
        .flat_map(|stream| stream.blocks.values())
        .map(|samples| samples.capacity() * std::mem::size_of::<f32>())
        .sum::<usize>();
    let stretch_bytes = cache
        .stretched
        .values()
        .filter_map(|stretch| match stretch {
            Stretch::Ready(samples) => Some(samples.allocated_bytes()),
            Stretch::Pending { .. } | Stretch::Rejected { .. } | Stretch::Failed => None,
        })
        .sum::<usize>();
    block_bytes + stretch_bytes
}

/// Seeds a cached mono block at a known timeline position for eviction tests.
fn seed_block(cache: &mut MixBufferCache, path: &str, index: u64, plays: u64) {
    let stream = cache.streams.entry((PathBuf::from(path), 0)).or_default();
    stream
        .blocks
        .insert(index, Arc::new(vec![0.0; BLOCK_FRAMES as usize]));
    stream.wanted.insert(index, plays);
}

/// Seeds a ready stretch buffer and its outer-timeline range for budget tests.
fn seed_ready_stretch(cache: &mut MixBufferCache, key: u64, frames: u64, plays: Range<u64>) {
    cache.stretched.insert(
        key,
        Stretch::Ready(SampleBlocks::contiguous(
            Arc::new(vec![0.0; frames as usize]),
            0,
            cache.channels,
        )),
    );
    cache.stretched_plays.insert(key, plays);
}

#[test]
/// Surplus blocks from a completed decode job must be evicted to the byte budget.
fn rebalance_evicts_surplus_blocks_left_by_a_completed_decode_job() {
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(MONO_BLOCK_BYTES);
    cache.set_playhead(0);
    let stream = cache
        .streams
        .entry((PathBuf::from("completed-job.wav"), 0))
        .or_default();
    stream.wanted.insert(0, 0);
    for index in 0..8 {
        stream
            .blocks
            .insert(index, Arc::new(vec![0.0; BLOCK_FRAMES as usize]));
    }

    cache.rebalance();

    assert_eq!(resident_audio_bytes(&cache), MONO_BLOCK_BYTES);
}

#[test]
/// Stretched buffers count toward the budget and compete with standard blocks.
fn rebalance_counts_stretched_audio_and_evicts_it_across_maps() {
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(2 * MONO_BLOCK_BYTES);
    cache.set_playhead(10 * BLOCK_FRAMES);
    seed_block(&mut cache, "near.wav", 0, 10 * BLOCK_FRAMES);
    let key = 42;
    seed_ready_stretch(
        &mut cache,
        key,
        2 * BLOCK_FRAMES,
        20 * BLOCK_FRAMES..22 * BLOCK_FRAMES,
    );

    cache.rebalance();

    assert!(matches!(
        cache.stretched.get(&key),
        Some(Stretch::Rejected { .. })
    ));
    assert_eq!(resident_audio_bytes(&cache), MONO_BLOCK_BYTES);
    assert_eq!(blocks_held(&cache, Path::new("near.wav")), [0]);
}

/// An oversized stretch leaves the active source blocks available as fallback audio.
#[test]
fn an_oversized_stretch_cannot_evict_active_playhead_blocks() {
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(2 * MONO_BLOCK_BYTES);
    let playhead = BLOCK_FRAMES / 2;
    cache.set_playhead(playhead);
    seed_block(&mut cache, "active.wav", 0, 0);
    seed_block(&mut cache, "active.wav", 1, BLOCK_FRAMES);
    let stream = cache
        .streams
        .get_mut(&(PathBuf::from("active.wav"), 0))
        .unwrap();
    for samples in stream.blocks.values_mut() {
        *samples = Arc::new(vec![0.5; BLOCK_FRAMES as usize]);
    }
    let key = 45;
    seed_ready_stretch(
        &mut cache,
        key,
        8 * BLOCK_FRAMES,
        playhead..playhead + 8 * BLOCK_FRAMES,
    );
    cache.stretch_wanted.insert(key);

    cache.rebalance();

    assert_eq!(blocks_held(&cache, Path::new("active.wav")), [0, 1]);
    assert!(matches!(
        cache.stretched.get(&key),
        Some(Stretch::Rejected { .. })
    ));
    let project = Project::default();
    let fallback = match cache.stretched(StretchRequest {
        project: &project,
        input: vv_audio::StretchInput::File {
            path: Path::new("active.wav"),
            stream: 0,
        },
        frames: 0..2 * BLOCK_FRAMES,
        plays: 0..2 * BLOCK_FRAMES,
        tempo: 1.0,
        sample_rate: 48_000,
        channels: 1,
        key,
    }) {
        ClipAudio::Partial(audio) => audio,
        _ => panic!("an oversized stretch should fall back to source audio"),
    };
    assert_eq!(fallback.frame(0), Some(&[0.5][..]));
    assert_eq!(fallback.frame(BLOCK_FRAMES), Some(&[0.5][..]));
    let audio = ready(&mut cache, Path::new("active.wav"), 0..2 * BLOCK_FRAMES);
    assert_eq!(audio.frame(0), Some(&[0.5][..]));
    assert_eq!(audio.frame(BLOCK_FRAMES), Some(&[0.5][..]));
}

/// A stretch using most of the budget remains cached and audible when it fits.
#[test]
fn a_long_stretch_larger_than_half_the_budget_remains_playable() {
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(4 * MONO_BLOCK_BYTES);
    let key = 46;
    let frames = 3 * BLOCK_FRAMES;
    cache.stretched.insert(
        key,
        Stretch::Ready(SampleBlocks::contiguous(
            Arc::new(vec![0.25; frames as usize]),
            0,
            1,
        )),
    );
    cache.stretched_plays.insert(key, 0..frames);
    cache.stretch_wanted.insert(key);

    cache.rebalance();

    assert!(matches!(cache.stretched.get(&key), Some(Stretch::Ready(_))));
    assert_eq!(resident_audio_bytes(&cache), 3 * MONO_BLOCK_BYTES);
    let project = Project::default();
    let audio = match cache.stretched(StretchRequest {
        project: &project,
        input: vv_audio::StretchInput::File {
            path: Path::new("long-stretch.wav"),
            stream: 0,
        },
        frames: 0..frames,
        plays: 0..frames,
        tempo: 1.0,
        sample_rate: 48_000,
        channels: 1,
        key,
    }) {
        ClipAudio::Ready(audio) => audio,
        _ => panic!("a stretch that fits the budget should remain available"),
    };
    assert_eq!(audio.frame(0), Some(&[0.25][..]));
    assert_eq!(audio.frame(frames - 1), Some(&[0.25][..]));
}

/// An over-budget stretch uses bounded varispeed source reads instead of muting the clip.
#[test]
fn an_over_budget_pitch_stretch_falls_back_to_audible_varispeed_audio() {
    let path = PathBuf::from("fallback-stretch.wav");
    let mut project = Project::default();
    let media = project.media_pool.insert(vv_core::MediaItem {
        path: path.clone(),
        meta: vv_core::MediaMeta {
            duration_frames: 100,
            fps: vv_core::Rational::new(25, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 48_000,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let mut clip = vv_core::Clip::new(
        vv_core::ClipId(1),
        vv_core::ClipSource::Media(media),
        0,
        0,
        50,
        vv_core::Rational::one(),
        vv_core::Rational::new(2, 1),
    );
    clip.pitch_correction = true;
    let timeline = vv_core::Timeline {
        name: "over-budget stretch".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1, 1),
        tracks: vec![vv_core::Track {
            kind: vv_core::TrackKind::Audio,
            clips: vec![clip],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        }],
        markers: Vec::new(),
        master: Default::default(),
    };
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(MONO_BLOCK_BYTES);
    cache.set_playhead(24_000);
    let stream = cache.streams.entry((path.clone(), 0)).or_default();
    stream
        .blocks
        .insert(0, Arc::new(vec![0.5; BLOCK_FRAMES as usize]));

    let snapshot = MixSnapshot::from_timeline(&project, &timeline, 48_000, 1, &mut cache);
    let mut output = vec![0.0; 1024];
    vv_audio::mixer::mix_range(&snapshot, 24_000, &mut output);

    assert!(
        output.iter().any(|&sample| sample > 0.4),
        "output peak {:?}, cached blocks {:?}",
        output.iter().copied().fold(0.0_f32, f32::max),
        cache.streams.get(&(path.clone(), 0)).map(|stream| stream
            .blocks
            .keys()
            .copied()
            .collect::<Vec<_>>())
    );
    assert!(cache.queued_stretches.is_empty());
    assert!(cache.stretched.is_empty());
}

/// Rebuilds a long fallback at each refresh point while playback advances.
#[test]
fn an_over_budget_fallback_window_slides_with_the_playhead() {
    let path = PathBuf::from("sliding-fallback.wav");
    let (project, timeline) = pitch_corrected_file_project(&path, 30);
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(MONO_BLOCK_BYTES);

    for playhead in [0, 2, 4, 6].map(|blocks| blocks * BLOCK_FRAMES) {
        let source_block = (playhead * 2) / BLOCK_FRAMES;
        cache.set_playhead(playhead);
        let refreshed = cache.poll();
        if playhead > 0 {
            assert!(refreshed, "fallback refresh was missed at {playhead}");
        }
        cache.begin_pass();
        let stream = cache.streams.entry((path.clone(), 0)).or_default();
        stream
            .blocks
            .insert(source_block, Arc::new(vec![0.5; BLOCK_FRAMES as usize]));

        let snapshot = MixSnapshot::from_timeline(&project, &timeline, 48_000, 1, &mut cache);
        let mut output = vec![0.0; 512];
        vv_audio::mixer::mix_range(&snapshot, playhead, &mut output);
        assert!(
            output.iter().any(|&sample| sample > 0.4),
            "fallback went silent at playhead {playhead}"
        );
        cache.sweep_unused();
        assert!(
            blocks_held(&cache, &path).contains(&source_block),
            "active source block {source_block} was not retained at {playhead}"
        );
    }
    assert!(cache.queued_stretches.is_empty());
}

/// Stretch reservations in one pass prevent later jobs from oversubscribing the budget.
#[test]
fn stretch_admission_accounts_for_pending_jobs_without_rendering_them() {
    let project = Project::default();
    let request = |key, name: &'static str| StretchRequest {
        project: &project,
        input: vv_audio::StretchInput::File {
            path: Path::new(name),
            stream: 0,
        },
        frames: 0..BLOCK_FRAMES,
        plays: 0..BLOCK_FRAMES,
        tempo: 1.0,
        sample_rate: 48_000,
        channels: 1,
        key,
    };
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(MONO_BLOCK_BYTES);
    cache.begin_pass();

    assert!(matches!(
        cache.stretched(request(50, "first.wav")),
        ClipAudio::Pending
    ));
    assert!(matches!(
        cache.stretched(request(51, "second.wav")),
        ClipAudio::Partial(_)
    ));
    assert_eq!(cache.queued_stretches.len(), 1);
}

/// A compound stretch falls back to a bounded nested mix under the same budget.
#[test]
fn an_over_budget_compound_stretch_falls_back_to_nested_audio() {
    let path = PathBuf::from("nested-fallback.wav");
    let (project, _, mut timeline) = compound_project(&path, 4);
    let clip = &mut timeline.tracks[0].clips[0];
    clip.set_speed(vv_core::Rational::new(2, 1), vv_core::Rational::one());
    clip.pitch_correction = true;
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(MONO_BLOCK_BYTES);
    cache
        .streams
        .entry((path, 0))
        .or_default()
        .blocks
        .insert(0, Arc::new(vec![0.5; BLOCK_FRAMES as usize]));

    let snapshot = MixSnapshot::from_timeline(&project, &timeline, 48_000, 1, &mut cache);
    let mut output = vec![0.0; 1024];
    vv_audio::mixer::mix_range(&snapshot, 0, &mut output);

    assert!(output.iter().any(|&sample| sample > 0.4));
    assert!(cache.stretched.is_empty());
}

/// Nested fallback requests and eviction priorities use the outer playhead clock.
#[test]
fn a_nested_fallback_retains_the_block_under_the_moving_playhead() {
    let path = PathBuf::from("nested-sliding-fallback.wav");
    let (project, _, mut timeline) = compound_project(&path, 30);
    let clip = &mut timeline.tracks[0].clips[0];
    clip.set_speed(vv_core::Rational::new(2, 1), vv_core::Rational::one());
    clip.pitch_correction = true;
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(MONO_BLOCK_BYTES);

    for playhead in [0, 2, 4, 6].map(|blocks| blocks * BLOCK_FRAMES) {
        let source_block = (playhead * 2) / BLOCK_FRAMES;
        cache.set_playhead(playhead);
        let refreshed = cache.poll();
        if playhead > 0 {
            assert!(
                refreshed,
                "nested fallback refresh was missed at {playhead}"
            );
        }
        cache.begin_pass();
        let stream = cache.streams.entry((path.clone(), 0)).or_default();
        stream
            .blocks
            .insert(source_block, Arc::new(vec![0.5; BLOCK_FRAMES as usize]));

        let snapshot = MixSnapshot::from_timeline(&project, &timeline, 48_000, 1, &mut cache);
        let mut output = vec![0.0; 512];
        vv_audio::mixer::mix_range(&snapshot, playhead, &mut output);
        assert!(
            output.iter().any(|&sample| sample > 0.4),
            "nested fallback went silent at playhead {playhead}"
        );
        cache.sweep_unused();
        let stream = &cache.streams[&(path.clone(), 0)];
        assert_eq!(
            stream.wanted.get(&source_block),
            Some(&playhead),
            "nested source block was not mapped onto the outer playback clock"
        );
        assert!(
            stream.blocks.contains_key(&source_block),
            "active nested source block {source_block} was evicted at {playhead}"
        );
    }
    assert!(cache.queued_stretches.is_empty());
}

/// Simultaneous blocks at the playhead remain subject to the shared byte limit.
#[test]
fn overlapping_playhead_blocks_cannot_exceed_the_shared_budget() {
    let mut cache = MixBufferCache::spawn(48_000, 1);
    let budget = 3 * MONO_BLOCK_BYTES;
    cache.set_budget_bytes(budget);
    let playhead = 100 * BLOCK_FRAMES + BLOCK_FRAMES / 2;
    cache.set_playhead(playhead);
    let plays = playhead - BLOCK_FRAMES / 2;
    let path = PathBuf::from("overlapping-streams.wav");
    for stream_index in 0..128 {
        let stream = cache
            .streams
            .entry((path.clone(), stream_index))
            .or_default();
        stream
            .blocks
            .insert(0, Arc::new(vec![0.0; BLOCK_FRAMES as usize]));
        stream.wanted.insert(0, plays);
    }

    cache.rebalance();

    assert!(cache.over_budget);
    assert!(resident_audio_bytes(&cache) <= budget);
    assert_eq!(
        cache
            .streams
            .values()
            .map(|stream| stream.blocks.len())
            .sum::<usize>(),
        3,
        "only the three blocks that fit the budget remain cached"
    );
}

#[test]
/// Cross-cache eviction preserves nearer audio and removes the farthest buffer.
fn cross_map_eviction_keeps_nearer_audio_and_drops_the_farthest_candidate() {
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(2 * MONO_BLOCK_BYTES);
    let playhead = 100 * BLOCK_FRAMES;
    cache.set_playhead(playhead);
    seed_block(&mut cache, "timeline.wav", 0, playhead);
    seed_block(&mut cache, "timeline.wav", 1, 130 * BLOCK_FRAMES);
    let key = 43;
    seed_ready_stretch(
        &mut cache,
        key,
        BLOCK_FRAMES,
        110 * BLOCK_FRAMES..111 * BLOCK_FRAMES,
    );

    cache.rebalance();

    assert_eq!(blocks_held(&cache, Path::new("timeline.wav")), [0]);
    assert!(cache.stretched.contains_key(&key));
    assert_eq!(resident_audio_bytes(&cache), 2 * MONO_BLOCK_BYTES);
}

#[test]
/// A rejected stretch is retried only after the budget can admit it.
fn rejected_stretch_is_not_queued_again_until_it_fits_the_budget() {
    let project = Project::default();
    let path = PathBuf::from("rejected-stretch.wav");
    let request = || StretchRequest {
        project: &project,
        input: vv_audio::StretchInput::File {
            path: &path,
            stream: 0,
        },
        frames: 0..BLOCK_FRAMES,
        plays: 0..BLOCK_FRAMES,
        tempo: 1.0,
        sample_rate: 48_000,
        channels: 1,
        key: 44,
    };
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(0);
    seed_ready_stretch(&mut cache, 44, BLOCK_FRAMES, 0..BLOCK_FRAMES);

    cache.rebalance();
    assert!(matches!(
        cache.stretched.get(&44),
        Some(Stretch::Rejected { .. })
    ));
    assert!(matches!(cache.stretched(request()), ClipAudio::Partial(_)));
    assert!(cache.queued_stretches.is_empty());

    cache.set_budget_bytes(3 * MONO_BLOCK_BYTES);
    assert!(
        cache.rebalance(),
        "admitting the stretch must rebuild the mix"
    );
    assert!(!cache.stretched.contains_key(&44));
    assert!(matches!(cache.stretched(request()), ClipAudio::Pending));
    assert_eq!(cache.queued_stretches.len(), 1);
}

#[test]
fn every_stream_of_a_file_starts_playing_before_any_of_them_is_fully_decoded() {
    let dir = std::env::temp_dir().join("vv-app-mix-buffers-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("three_streams_long.mkv");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=120",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=660:sample_rate=48000:duration=120",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=48000:duration=120",
            "-map",
            "0:a",
            "-map",
            "1:a",
            "-map",
            "2:a",
            "-c:a",
            "aac",
        ],
        &path,
    );

    // The order of the decoder's messages, not a poll loop racing it: a
    // fast decode would finish before the loop looked.
    let blocks = 0..(121 * SEC).div_ceil(BLOCK_FRAMES);
    let jobs: Vec<DecodeJob> = (0..3)
        .map(|stream| DecodeJob {
            id: stream as u64,
            path: path.clone(),
            stream,
            blocks: blocks.clone(),
            plays: 0..blocks.end * BLOCK_FRAMES,
            cancelled: Arc::default(),
        })
        .collect();
    let (tx, rx) = mpsc::channel();
    assert!(decode_blocks(&path, &jobs, 48_000, 2, &tx));
    drop(tx);
    let messages: Vec<Decoded> = rx.iter().collect();
    let first_done = messages
        .iter()
        .position(|m| matches!(m, Decoded::Done { .. }))
        .unwrap();
    for stream in 0..3 {
        assert!(
            messages[..first_done]
                .iter()
                .any(|m| matches!(m, Decoded::Block { job, .. } if *job == stream)),
            "stream {stream} only arrived at the end"
        );
    }
    assert!(messages.iter().all(|m| match m {
        Decoded::Block { samples, .. } => samples.len() as u64 <= BLOCK_FRAMES * 2,
        Decoded::Done { end, .. } => end.is_some_and(|end| end.abs_diff(120 * SEC) < 2048),
    }));
}

#[test]
/// A ready stretch is computed on a worker and released after it is unused.
fn a_stretch_is_computed_in_background_and_dropped_once_unused() {
    let path = sine_wav("stretched.wav", 3);
    let budget = 5 * MONO_BLOCK_BYTES / 2;
    let project = Project::default();
    let request = || StretchRequest {
        project: &project,
        input: vv_audio::StretchInput::File {
            path: &path,
            stream: 0,
        },
        frames: 0..2 * SEC,
        plays: 0..2 * SEC,
        tempo: 1.0,
        sample_rate: 48_000,
        channels: 1,
        key: 1,
    };
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(budget);
    let ask = |cache: &mut MixBufferCache| cache.stretched(request());
    assert!(matches!(ask(&mut cache), ClipAudio::Pending));
    cache.send_jobs(&project);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let stretched = loop {
        cache.poll();
        match ask(&mut cache) {
            ClipAudio::Ready(samples) => break samples,
            ClipAudio::Pending => {}
            _ => panic!("the stretch failed"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "stretch never arrived"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let secs = stretched.end() as f64 / 48_000.0;
    assert!((secs - 2.0).abs() < 0.05, "the stretch lasts {secs} s");
    assert!(resident_audio_bytes(&cache) <= budget);
    assert!(
        blocks_held(&cache, &path).is_empty(),
        "read from the file, not the preview's blocks"
    );

    cache.sweep_unused();
    assert!(matches!(ask(&mut cache), ClipAudio::Ready(_)), "still used");
    cache.sweep_unused();
    cache.sweep_unused();
    assert!(
        matches!(ask(&mut cache), ClipAudio::Pending),
        "dropped when unused"
    );
}

#[test]
fn only_the_blocks_read_are_decoded_with_the_same_samples_as_the_whole_file() {
    let path = sine_wav("ranged.wav", 60);
    let mut cache = MixBufferCache::spawn(48_000, 1);
    let frames = 20 * SEC..25 * SEC;
    let ClipAudio::Partial(decoding) = read(&mut cache, &path, frames.clone()) else {
        panic!("still decoding");
    };
    assert!(!decoding.holds(frames.clone()));
    wait_done(&mut cache);

    let buffer = ready(&mut cache, &path, frames.clone());
    let (first, last) = (
        frames.start / BLOCK_FRAMES,
        frames.end.div_ceil(BLOCK_FRAMES),
    );
    let with_margin: Vec<u64> = (first - MARGIN_BLOCKS..last + MARGIN_BLOCKS).collect();
    assert_eq!(blocks_held(&cache, &path), with_margin);
    let whole = vv_media::decode_audio_track(&path, 0).unwrap().unwrap();
    assert_eq!(
        buffer.to_vec(frames.clone()),
        whole.samples[frames.start as usize..frames.end as usize]
    );
}

#[test]
fn a_trim_into_the_margin_is_ready_at_once() {
    let path = sine_wav("within.wav", 40);
    let mut cache = MixBufferCache::spawn(48_000, 1);
    read(&mut cache, &path, 10 * SEC..12 * SEC);
    wait_done(&mut cache);

    let trimmed = 10 * SEC - BLOCK_FRAMES / 2..12 * SEC + BLOCK_FRAMES / 2;
    ready(&mut cache, &path, trimmed);
}

#[test]
fn a_longer_read_plays_the_blocks_there_while_the_others_decode() {
    let path = sine_wav("extended.wav", 40);
    let mut cache = MixBufferCache::spawn(48_000, 1);
    read(&mut cache, &path, 10 * SEC..12 * SEC);
    wait_done(&mut cache);

    let held = blocks_held(&cache, &path).len();
    assert!(matches!(
        read(&mut cache, &path, 10 * SEC..30 * SEC),
        ClipAudio::Partial(_)
    ));
    wait_done(&mut cache);
    ready(&mut cache, &path, 10 * SEC..30 * SEC);
    assert!(blocks_held(&cache, &path).len() > held);
}

#[test]
fn a_pass_frees_the_blocks_a_shortened_clip_no_longer_reads() {
    let path = sine_wav("shortened.wav", 40);
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.begin_pass();
    read(&mut cache, &path, 0..40 * SEC);
    cache.sweep_unused();
    wait_done(&mut cache);

    cache.begin_pass();
    ready(&mut cache, &path, 0..5 * SEC);
    cache.sweep_unused();
    let kept = (5 * SEC).div_ceil(BLOCK_FRAMES) + MARGIN_BLOCKS;
    assert_eq!(blocks_held(&cache, &path), (0..kept).collect::<Vec<_>>());
}

#[test]
fn a_read_past_the_end_of_the_file_ends_at_it() {
    let path = sine_wav("short.wav", 10);
    let mut cache = MixBufferCache::spawn(48_000, 1);
    read(&mut cache, &path, 5 * SEC..20 * SEC);
    wait_done(&mut cache);

    let buffer = ready(&mut cache, &path, 5 * SEC..20 * SEC);
    assert_eq!(buffer.end(), 10 * SEC);
}

#[test]
fn what_a_pass_does_not_read_is_dropped() {
    let path = sine_wav("dropped.wav", 10);
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.begin_pass();
    read(&mut cache, &path, 0..SEC);
    cache.sweep_unused();
    wait_done(&mut cache);
    assert!(!cache.streams.is_empty());

    cache.begin_pass();
    cache.sweep_unused();
    assert!(cache.streams.is_empty(), "the file's audio is freed");
}

/// Rebuilds `parent`'s mix as the preview does until nothing is pending.
fn settle(
    cache: &mut MixBufferCache,
    project: &Project,
    parent: &vv_core::Timeline,
) -> MixSnapshot {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "the mix never settled"
        );
        cache.begin_pass();
        let snapshot = MixSnapshot::from_timeline(project, parent, 48_000, 1, cache);
        cache.sweep_unused();
        cache.send_jobs(project);
        if !cache.has_pending() {
            cache.begin_pass();
            let snapshot = MixSnapshot::from_timeline(project, parent, 48_000, 1, cache);
            cache.sweep_unused();
            cache.send_jobs(project);
            if !cache.has_pending() {
                return snapshot;
            }
        }
        drop(snapshot);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn a_compound_mixed_live_plays_as_over_whole_files() {
    let path = sine_wav("compound_blocks.wav", 6);
    let (project, _, parent) = compound_project(&path, 6);
    let mut cache = MixBufferCache::spawn(48_000, 1);
    let snapshot = settle(&mut cache, &project, &parent);

    let whole_audio = vv_media::decode_audio_track(&path, 0).unwrap().unwrap();
    let whole = SampleBlocks::contiguous(Arc::new(whole_audio.samples), 0, 1);
    let mut source = |_: &Path, _: usize| Some(whole.clone());
    let reference = MixSnapshot::from_timeline(&project, &parent, 48_000, 1, &mut source);
    let mut got = vec![0.0; 6 * SEC as usize];
    let mut want = got.clone();
    vv_audio::mixer::mix_range(&snapshot, 0, &mut got);
    vv_audio::mixer::mix_range(&reference, 0, &mut want);
    assert_eq!(got, want);
}

/// Over budget the preview holds part of the audio: the level is measured
/// from the files all the same (it used to wait for all of it, forever).
#[test]
fn a_master_normalization_is_measured_over_budget() {
    let path = sine_wav("normalized_over_budget.wav", 6);
    let (project, _, mut parent) = compound_project(&path, 6);
    parent.master.effects = vec![vv_core::AudioEffect::new(
        vv_core::AudioEffectKind::Normalize {
            target_db: 0.0,
            mode: vv_core::NormalizeMode::SamplePeak,
            set_level: vv_core::SetLevel::Relative,
        },
    )];
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.set_budget_bytes(BLOCK_FRAMES as usize * 4);
    let snapshot = settle(&mut cache, &project, &parent);
    assert!(cache.over_budget);
    let vv_audio::mixer::Insert::Gain(gain) = snapshot.master.inserts[0] else {
        panic!("{:?}", snapshot.master.inserts);
    };
    assert!(gain > 1.01, "measured: {gain}");
}

fn job(id: u64, path: &str, stream: usize, blocks: Range<u64>, plays_at: u64) -> DecodeJob {
    let plays = plays_at..plays_at + (blocks.end - blocks.start) * BLOCK_FRAMES;
    DecodeJob {
        id,
        path: PathBuf::from(path),
        stream,
        blocks,
        plays,
        cancelled: Arc::default(),
    }
}

#[test]
fn the_job_holding_the_playhead_goes_before_the_next_one_whose_start_is_closer() {
    let mut queue = vec![
        job(0, "a.wav", 0, 32..40, 32 * BLOCK_FRAMES),
        job(1, "a.wav", 0, 24..32, 24 * BLOCK_FRAMES),
    ];
    let playhead = 29 * BLOCK_FRAMES + BLOCK_FRAMES / 2;
    let first = take_batch(&mut queue, playhead);
    assert_eq!(first[0].id, 1);
}

#[test]
fn a_read_takes_the_other_streams_queued_over_the_same_blocks() {
    let mut queue = vec![
        job(0, "a.wav", 0, 10..18, 0),
        job(1, "b.wav", 0, 10..18, 10),
        job(2, "a.wav", 1, 10..18, 50),
        job(3, "a.wav", 0, 40..48, 60),
    ];
    let mut ids: Vec<u64> = take_batch(&mut queue, 0).iter().map(|job| job.id).collect();
    ids.sort_unstable();
    assert_eq!(ids, [0, 2]);
    assert_eq!(queue.len(), 2);
}

#[test]
fn a_resampled_file_read_in_distant_places_matches_its_whole_decode() {
    let dir = std::env::temp_dir().join("vv-app-mix-buffers-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("chirp44.wav");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.5*sin(2*PI*(100+300*t)*t):duration=60:sample_rate=44100",
        ],
        &path,
    );
    let mut cache = MixBufferCache::spawn(48_000, 1);
    let early = 5 * SEC..8 * SEC;
    let late = 50 * SEC..53 * SEC;
    read(&mut cache, &path, early.clone());
    read(&mut cache, &path, late.clone());
    wait_done(&mut cache);

    let mut whole = Vec::new();
    vv_media::decode_audio_streams_streaming(&path, &[0], Some(48_000), |_, _, chunk| {
        whole.extend_from_slice(chunk);
        ControlFlow::Continue(())
    })
    .unwrap();
    for frames in [early, late] {
        let buffer = ready(&mut cache, &path, frames.clone());
        assert_eq!(
            buffer.to_vec(frames.clone()),
            whole[frames.start as usize..frames.end as usize]
        );
    }
}

/// Ten minutes at 2x: a stretch that takes far longer than the tests wait.
/// Builds a long file-backed stretch request for worker lifecycle tests.
fn long_stretch<'a>(project: &'a Project, path: &'a Path, key: u64) -> StretchRequest<'a> {
    StretchRequest {
        project,
        input: vv_audio::StretchInput::File { path, stream: 0 },
        frames: 0..600 * SEC,
        plays: 0..300 * SEC,
        tempo: 2.0,
        sample_rate: 48_000,
        channels: 1,
        key,
    }
}

fn wait_idle(busy: &AtomicUsize, within: std::time::Duration) {
    let deadline = std::time::Instant::now() + within;
    while busy.load(Ordering::Relaxed) > 0 {
        assert!(std::time::Instant::now() < deadline, "still busy");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// A drag of a pitch-corrected clip asks a new stretch every frame.
#[test]
fn a_stretch_no_longer_played_stops_and_the_next_waits_for_the_worker() {
    let path = sine_wav("long_stretch.wav", 600);
    let project = Project::default();
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.begin_pass();
    cache.stretched(long_stretch(&project, &path, 1));
    cache.sweep_unused();
    cache.send_jobs(&project);

    cache.begin_pass();
    cache.stretched(long_stretch(&project, &path, 2));
    cache.sweep_unused();
    cache.send_jobs(&project);
    assert_eq!(
        cache.queued_stretches.len(),
        1,
        "held while the worker is busy"
    );

    wait_idle(&cache.stretch_busy, std::time::Duration::from_secs(5));
    cache.send_jobs(&project);
    assert!(cache.queued_stretches.is_empty());
}

#[test]
fn dropping_the_cache_stops_the_stretches_and_measures_running() {
    let path = sine_wav("long_drop.wav", 600);
    let (project, _, mut parent) = compound_project(&path, 600);
    parent.master.effects = vec![vv_core::AudioEffect::new(
        vv_core::AudioEffectKind::Normalize {
            target_db: 0.0,
            mode: vv_core::NormalizeMode::Loudness,
            set_level: vv_core::SetLevel::Relative,
        },
    )];
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.begin_pass();
    MixSnapshot::from_timeline(&project, &parent, 48_000, 1, &mut cache);
    cache.stretched(long_stretch(&project, &path, 1));
    cache.sweep_unused();
    cache.send_jobs(&project);
    assert_eq!(cache.level_busy.load(Ordering::Relaxed), 1);
    assert_eq!(cache.stretch_busy.load(Ordering::Relaxed), 1);

    let started = std::time::Instant::now();
    drop(cache);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "dropped in {:?}",
        started.elapsed()
    );
}

#[test]
fn previewing_a_media_keeps_the_timeline_blocks_and_not_the_previous_media() {
    let timeline = sine_wav("timeline_kept.wav", 10);
    let first = sine_wav("first_media.wav", 10);
    let second = sine_wav("second_media.wav", 10);
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.begin_pass();
    read(&mut cache, &timeline, 0..5 * SEC);
    cache.sweep_unused();
    wait_done(&mut cache);
    let held = blocks_held(&cache, &timeline);
    assert!(!held.is_empty());

    for media in [&first, &second] {
        cache.begin_media_pass();
        cache.media_frames(media, 0, 0..5 * SEC);
        cache.sweep_unused();
        wait_done(&mut cache);
    }
    assert_eq!(blocks_held(&cache, &timeline), held);
    assert!(blocks_held(&cache, &first).is_empty());

    cache.begin_pass();
    ready(&mut cache, &timeline, 0..5 * SEC);
    cache.sweep_unused();
    assert!(blocks_held(&cache, &second).is_empty());
}

fn analysis<'a>(
    project: &'a Project,
    timeline: &'a vv_core::Timeline,
    key: u64,
) -> LevelAnalysis<'a> {
    LevelAnalysis {
        slot: None,
        project,
        timeline,
        measured: vv_audio::Measured {
            channel: vv_core::MixerChannel::Master,
            insert: 0,
            clip: None,
        },
        mode: vv_core::NormalizeMode::SamplePeak,
        sample_rate: 48_000,
        channels: 1,
        segments: vec![LevelSegment {
            frames: 0..10,
            preroll: 0,
            key,
        }],
        dry: Vec::new(),
        scope: 0,
    }
}

#[test]
fn a_rescan_measures_again_what_was_rendered_while_a_plugin_was_not_made() {
    let mut cache = MixBufferCache::spawn(48_000, 1);
    let (project, timeline) = (
        Project::default(),
        vv_core::Timeline {
            name: String::new(),
            fps: vv_core::Rational::new(25, 1),
            resolution: (4, 2),
            tracks: Vec::new(),
            markers: Vec::new(),
            master: Default::default(),
        },
    );
    let (bad, good) = (
        analysis(&project, &timeline, 1),
        analysis(&project, &timeline, 2),
    );
    let (bad_key, good_key) = (bad.key(), good.key());
    cache.levels.insert(bad_key, Some(1.0));
    cache.levels.insert(good_key, Some(2.0));
    let blocks = SampleBlocks::contiguous(Arc::new(Vec::new()), 0, 1);
    cache.stretched.insert(10, Stretch::Ready(blocks.clone()));
    cache.stretched.insert(11, Stretch::Ready(blocks));
    cache.stretched.insert(
        12,
        Stretch::Pending {
            cancelled: Arc::default(),
            bytes: 0,
        },
    );
    cache.degraded.extend([bad_key, 10, 12]);

    cache.clear_plugin_failures();

    assert!(!cache.levels.contains_key(&bad_key));
    assert_eq!(cache.levels[&good_key], Some(2.0));
    assert!(!cache.stretched.contains_key(&10));
    assert!(cache.stretched.contains_key(&11));
    assert!(matches!(cache.stretched[&12], Stretch::Pending { .. }));
    assert!(cache.degraded.is_empty());
    assert!(matches!(cache.level(bad), LevelReading::Pending(None)));
    assert_eq!(cache.queued_levels.len(), 1);

    cache.degraded.extend([bad_key, 99]);
    cache.sweep_unused();
    assert_eq!(cache.degraded, HashSet::from([bad_key]));
}

#[test]
fn a_degraded_result_from_before_a_rescan_is_asked_again() {
    let mut cache = MixBufferCache::spawn(48_000, 1);
    let (level_tx, level_rx) = mpsc::channel();
    let (stretch_tx, stretch_rx) = mpsc::channel();
    cache.level_rx = level_rx;
    cache.stretch_rx = stretch_rx;
    cache.clear_plugin_failures();
    let generation = cache.retry.load(Ordering::Relaxed);
    for key in [1, 2] {
        cache.levels.insert(key, None);
    }
    cache.stretched.insert(
        3,
        Stretch::Pending {
            cancelled: Arc::default(),
            bytes: 0,
        },
    );
    cache.stretched.insert(
        4,
        Stretch::Pending {
            cancelled: Arc::default(),
            bytes: 0,
        },
    );
    let blocks = SampleBlocks::contiguous(Arc::new(Vec::new()), 0, 1);
    level_tx
        .send((1, None, Some(1.0), Some(generation - 1)))
        .unwrap();
    level_tx
        .send((2, None, Some(1.0), Some(generation)))
        .unwrap();
    stretch_tx
        .send((3, Ok(blocks.clone()), Some(generation - 1)))
        .unwrap();
    stretch_tx.send((4, Ok(blocks), Some(generation))).unwrap();

    cache.poll();

    assert!(!cache.levels.contains_key(&1));
    assert_eq!(cache.levels[&2], Some(1.0));
    assert!(!cache.stretched.contains_key(&3));
    assert!(matches!(cache.stretched[&4], Stretch::Ready(_)));
    assert_eq!(cache.degraded, HashSet::from([2, 4]));
}

fn queued(slot: AnalysisSlot, key: u64) -> OwnedAnalysis {
    OwnedAnalysis {
        key,
        slot: Some(slot),
        timeline: queued_timeline(),
        measured: Measured {
            channel: vv_core::MixerChannel::Master,
            insert: 0,
            clip: None,
        },
        mode: NormalizeMode::SamplePeak,
        format: (48_000, 1),
        segments: Vec::new(),
        dry: Vec::new(),
        scope: 0,
    }
}

#[test]
fn only_an_outdated_level_in_flight_is_stopped_by_a_newer_one_of_its_slot() {
    let mut project = Project::default();
    let id = project.timelines.insert(queued_timeline());
    let slot = (id, vv_core::MixerChannel::Master, 0, 0);
    let other = (id, vv_core::MixerChannel::Master, 1, 0);
    let aborts = |inflight: (AnalysisSlot, u64), wanted: &[u64], queue: OwnedAnalysis| {
        let mut cache = MixBufferCache::spawn(48_000, 1);
        *cache.level_inflight.lock().unwrap() = Some(inflight);
        cache.levels.extend(wanted.iter().map(|&k| (k, None)));
        cache.queued_levels.push(queue);
        cache.abort_outdated_level();
        cache.level_abort.load(Ordering::SeqCst)
    };
    assert!(aborts((slot, 1), &[2], queued(slot, 2)), "outdated");
    assert!(!aborts((slot, 1), &[1, 2], queued(slot, 2)), "still wanted");
    assert!(!aborts((slot, 1), &[2], queued(other, 2)), "another slot");
}

fn queued_timeline() -> vv_core::Timeline {
    vv_core::Timeline {
        name: String::new(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1, 1),
        tracks: Vec::new(),
        markers: Vec::new(),
        master: Default::default(),
    }
}

struct Stub;

impl vv_audio::PluginProcessor for Stub {
    fn process(&self, _: &mut [f32], _: usize) -> bool {
        true
    }
    fn reset(&self) {}
    fn latency(&self) -> u32 {
        0
    }
}

impl vv_plugin::HostedPlugin for Stub {
    fn set_state(&self, _: &vv_core::Vst3Effect) {}
    fn state_hash(&self) -> u64 {
        0
    }
    fn mark_state(&self, _: u64) {}
}

fn listed(class_id: &str, path: &str) -> vv_plugin::PluginInfo {
    vv_plugin::PluginInfo {
        class_id: class_id.into(),
        path: path.into(),
        ..vv_plugin::testing::fixture_info()
    }
}

fn effect_of(class_id: &str) -> vv_core::Vst3Effect {
    vv_plugin::testing::fixture_effect_of(class_id, &[])
}

#[test]
fn a_rescan_with_one_more_plugin_keeps_the_live_instances() {
    let mut cache = MixBufferCache::spawn(48_000, 1);
    cache.plugins = Some(vv_plugin::Instances::with_maker(
        vec![listed("A", "/a.vst3")].into(),
        Box::new(|_, _, _| Some(Arc::new(Stub))),
    ));
    let effect = effect_of("A");
    let request = |effect| PluginRequest {
        key: 1,
        effect,
        sample_rate: 48_000,
        process: true,
    };
    let plugins = cache.plugins.as_mut().unwrap();
    plugins.begin_pass();
    plugins.get(request(&effect)).unwrap();
    assert!(cache.plugin_latency(1).is_some());
    let generation = cache.retry.load(Ordering::Relaxed);

    let dropped =
        cache.refresh_plugins(vec![listed("A", "/a.vst3"), listed("B", "/b.vst3")].into());

    assert!(dropped.is_empty());
    assert!(cache.plugin_latency(1).is_some());
    assert!(cache.plugins.as_ref().unwrap().knows(&effect_of("B")));
    assert_eq!(cache.retry.load(Ordering::Relaxed), generation + 1);
}
