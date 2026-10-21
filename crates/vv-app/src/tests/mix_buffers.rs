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
fn a_stretch_is_computed_in_background_and_dropped_once_unused() {
    let path = sine_wav("stretched.wav", 2);
    let project = Project::default();
    let request = || StretchRequest {
        project: &project,
        input: vv_audio::StretchInput::File {
            path: &path,
            stream: 0,
        },
        frames: 0..48_000,
        tempo: 2.0,
        pitch: 1.0,
        preserve_formants: false,
        sample_rate: 48_000,
        channels: 1,
        key: 1,
    };
    let mut cache = MixBufferCache::spawn(48_000, 1);
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
    assert!((secs - 0.5).abs() < 0.05, "1 s at 2x lasts {secs} s");
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
fn a_pitch_shift_is_prepared_in_background_at_the_original_duration() {
    let path = sine_wav("pitch_shift.wav", 2);
    let project = Project::default();
    let request = || StretchRequest {
        project: &project,
        input: vv_audio::StretchInput::File {
            path: &path,
            stream: 0,
        },
        frames: 0..48_000,
        tempo: 1.0,
        pitch: 2.0,
        preserve_formants: true,
        sample_rate: 48_000,
        channels: 1,
        key: 42,
    };
    let mut cache = MixBufferCache::spawn(48_000, 1);
    assert!(matches!(cache.stretched(request()), ClipAudio::Pending));
    cache.send_jobs(&project);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let shifted = loop {
        cache.poll();
        match cache.stretched(request()) {
            ClipAudio::Ready(samples) => break samples,
            ClipAudio::Pending => {}
            _ => panic!("pitch shift failed"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pitch shift never arrived"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(shifted.end().abs_diff(48_000) < 2048);
    let middle = shifted.to_vec(12_000..36_000);
    let crossings = middle
        .windows(2)
        .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
        .count();
    let frequency = crossings as f64 * 48_000.0 / middle.len() as f64;
    assert!((frequency - 880.0).abs() < 8.0, "heard {frequency} Hz");
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
fn long_stretch<'a>(project: &'a Project, path: &'a Path, key: u64) -> StretchRequest<'a> {
    StretchRequest {
        project,
        input: vv_audio::StretchInput::File { path, stream: 0 },
        frames: 0..600 * SEC,
        tempo: 2.0,
        pitch: 1.0,
        preserve_formants: false,
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
