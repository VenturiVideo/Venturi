use super::*;

const FPS: f64 = 25.0;
const RATE: f64 = PROJECT_SAMPLE_RATE as f64;

fn secs(audio: &TimelineAudio) -> f64 {
    audio.position_sample() as f64 / RATE
}

fn wait_for_speed(audio: &mut TimelineAudio, speed: f64) -> Duration {
    let started = Instant::now();
    while audio.speed() != speed {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the stretch should have completed"
        );
        std::thread::sleep(Duration::from_millis(5));
        audio.tick();
    }
    started.elapsed()
}

#[test]
fn seek_and_position_round_trip_in_timeline_frames() {
    let mut audio = TimelineAudio::new();
    for frame in [0, 1, 24, 250, 1234] {
        audio.seek_frame(frame, FPS);
        assert_eq!(audio.position_frame(FPS), frame);
    }
}

#[test]
fn clock_advances_while_playing_and_stops_on_pause() {
    let mut audio = TimelineAudio::new();
    audio.seek_frame(100, FPS);
    audio.play();
    std::thread::sleep(Duration::from_millis(300));
    let playing_pos = audio.position_frame(FPS);
    assert!((103..=112).contains(&playing_pos), "pos={playing_pos}");

    audio.pause();
    let paused_pos = audio.position_frame(FPS);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(audio.position_frame(FPS), paused_pos);
    assert!(paused_pos >= playing_pos);
}

/// Until the first window is ready playback stays at 1x; when it arrives the
/// playhead continues from where it got to, not from where it was at the request.
#[test]
fn fast_forward_applies_when_the_first_window_is_ready_without_jumping_back() {
    if vv_audio::stretch::skip_without_rubberband() {
        return;
    }
    let mut audio = TimelineAudio::new();
    audio.play();
    std::thread::sleep(Duration::from_millis(100));
    let at_request = secs(&audio);
    audio.request_speed(2.0);
    assert_eq!(audio.speed(), 1.0);

    let wait = wait_for_speed(&mut audio, 2.0);
    assert!(wait < Duration::from_secs(2), "wait={wait:?}");
    let at_apply = secs(&audio);
    assert!(
        at_apply >= at_request + wait.as_secs_f64() - 0.05,
        "at_request={at_request} wait={wait:?} at_apply={at_apply}"
    );

    std::thread::sleep(Duration::from_millis(300));
    let advanced = secs(&audio) - at_apply;
    assert!(advanced > 0.45 && advanced < 0.9, "advanced={advanced}");

    audio.request_speed(1.0);
    assert_eq!(audio.speed(), 1.0, "going back to 1x is immediate");
    assert!(audio.stretched.is_none());
}

fn assert_window_extends_seamlessly(tempo: f64) {
    let mut audio = TimelineAudio::new();
    audio.play();
    audio.request_speed(tempo);
    wait_for_speed(&mut audio, tempo);

    let window_secs = WINDOW_FRAMES as f64 / RATE;
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut max = 0.0;
    while Instant::now() < deadline && max <= window_secs + 1.0 {
        audio.tick();
        let pos = secs(&audio);
        assert!(pos >= max - 1e-6, "never backwards: pos={pos} max={max}");
        max = pos;
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(max > window_secs + 1.0, "max={max}");
    let window = audio.stretched.as_ref().unwrap();
    let covered = window.covered_until(audio.channels()) as f64 / RATE;
    assert!(
        covered > max,
        "audio must lead the playhead: covered={covered} max={max}"
    );
}

#[test]
fn fast_forward_window_extends_seamlessly_at_4x() {
    if vv_audio::stretch::skip_without_rubberband() {
        return;
    }
    assert_window_extends_seamlessly(4.0);
}

#[test]
fn fast_forward_window_extends_seamlessly_at_8x() {
    if vv_audio::stretch::skip_without_rubberband() {
        return;
    }
    assert_window_extends_seamlessly(8.0);
}

/// Seek outside the window during fast forward: restarts from there, without
/// going back to 1x nor stalling.
#[test]
fn seeking_outside_the_window_restarts_fast_forward_from_there() {
    if vv_audio::stretch::skip_without_rubberband() {
        return;
    }
    let mut audio = TimelineAudio::new();
    audio.play();
    audio.request_speed(4.0);
    wait_for_speed(&mut audio, 4.0);

    let target = 60 * PROJECT_SAMPLE_RATE as u64;
    audio.seek_sample(target);
    assert_eq!(audio.speed(), 4.0);
    assert!(secs(&audio) >= 60.0);

    let started = Instant::now();
    while audio.stretched.as_ref().unwrap().chunks.is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "window never arrived"
        );
        std::thread::sleep(Duration::from_millis(5));
        audio.tick();
    }
    assert_eq!(audio.stretched.as_ref().unwrap().origin, target);
    let pos = secs(&audio);
    assert!(pos > 60.0 && pos < 63.0, "pos={pos}");
}

/// A click seeks a few times to the same place: the stretch already on its
/// way from there is kept, not thrown away and started again.
#[test]
fn seeking_again_to_the_same_place_keeps_the_stretch_in_flight() {
    if vv_audio::stretch::skip_without_rubberband() {
        return;
    }
    let mut audio = TimelineAudio::new();
    audio.play();
    audio.request_speed(2.0);
    wait_for_speed(&mut audio, 2.0);

    let target = 60 * PROJECT_SAMPLE_RATE as u64;
    audio.seek_sample(target);
    let request = audio.stretch_request.expect("a window wanted from there");
    assert!(request.new_window);
    audio.seek_sample(target);
    audio.seek_sample(target);
    assert_eq!(audio.stretch_request.map(|r| r.id), Some(request.id));
    assert_eq!(audio.restretch, None);
}

/// The first window is short to sound at once; the extensions double.
#[test]
fn fast_forward_windows_start_short_and_double() {
    if vv_audio::stretch::skip_without_rubberband() {
        return;
    }
    let mut audio = TimelineAudio::new();
    audio.play();
    audio.request_speed(2.0);

    let mut sizes: Vec<u64> = Vec::new();
    let started = Instant::now();
    while sizes.last() != Some(&WINDOW_FRAMES) {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "sizes={sizes:?}"
        );
        if let Some(request) = audio.stretch_request
            && sizes.len() <= request.id as usize
        {
            sizes.push(request.frames);
        }
        std::thread::sleep(Duration::from_millis(2));
        audio.tick();
    }
    assert_eq!(
        sizes,
        [FIRST_WINDOW_FRAMES, 2 * FIRST_WINDOW_FRAMES, WINDOW_FRAMES]
    );
}

#[test]
fn a_superseded_stretch_result_is_discarded() {
    if vv_audio::stretch::skip_without_rubberband() {
        return;
    }
    let mut audio = TimelineAudio::new();
    audio.play();
    audio.request_speed(2.0);
    audio.request_speed(4.0);
    wait_for_speed(&mut audio, 4.0);
    std::thread::sleep(Duration::from_millis(500));
    audio.tick();
    assert_eq!(audio.speed(), 4.0);
    assert_eq!(audio.stretched.as_ref().unwrap().tempo, 4);
}

#[test]
fn scrub_snippet_does_not_move_the_position() {
    let mut audio = TimelineAudio::new();
    audio.seek_frame(50, FPS);
    audio.play_scrub_snippet();
    assert!(!audio.is_playing());
    std::thread::sleep(SCRUB_SNIPPET + Duration::from_millis(50));
    audio.tick();
    assert!(!audio.is_scrub_snippet_active());
    assert_eq!(audio.position_frame(FPS), 50);
}

/// A timeline playing `tone.wav` for 20 s.
fn tone_timeline(project: &mut vv_core::Project, name: &str, amplitude: f32) -> TimelineId {
    use vv_core::{
        Clip, ClipId, ClipSource, MediaItem, MediaMeta, Rational, Timeline, Track, TrackKind,
    };
    let dir = std::env::temp_dir().join("vv-app-timeline-audio-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let lavfi = format!("aevalsrc={amplitude}*sin(2*PI*440*t):sample_rate=48000:duration=20");
    vv_media::test_support::ffmpeg(&["-f", "lavfi", "-i", &lavfi], &path);
    let media = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 500,
            fps: Rational::new(25, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 48_000,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: amplitude.to_bits() as u64,
        compound: None,
        folder: None,
    });
    let clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(media),
        0,
        500,
        0,
        Rational::one(),
    );
    project.timelines.insert(Timeline {
        name: name.into(),
        fps: Rational::new(25, 1),
        resolution: (4, 2),
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new(TrackKind::Audio)
        }],
        markers: Vec::new(),
        master: Default::default(),
    })
}

/// Syncs `timeline` until the fast forward window is loud, within `limit`.
fn wait_for_loud_window(
    audio: &mut TimelineAudio,
    project: &vv_core::Project,
    timeline: TimelineId,
    limit: Duration,
) -> Duration {
    let started = Instant::now();
    while audio.stretched_peak().unwrap_or(0.0) < 0.5 {
        assert!(
            started.elapsed() < limit,
            "still {:?} after {limit:?}",
            audio.stretched_peak()
        );
        audio.sync(project, timeline, 1);
        audio.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
    started.elapsed()
}

/// Opening another timeline (a compound clip) during a fast forward: its
/// window was stretched from the timeline left, and played it for seconds.
#[test]
fn another_timeline_during_a_fast_forward_plays_its_own_audio_at_once() {
    if vv_audio::stretch::skip_without_rubberband() {
        return;
    }
    let mut project = vv_core::Project::default();
    let quiet = tone_timeline(&mut project, "quiet.wav", 0.05);
    let loud = tone_timeline(&mut project, "loud.wav", 0.8);
    let mut audio = TimelineAudio::new();
    while {
        audio.sync(&project, quiet, 1);
        audio.has_pending_buffers()
    } {
        std::thread::sleep(Duration::from_millis(10));
    }
    audio.play();
    audio.request_speed(2.0);
    wait_for_speed(&mut audio, 2.0);
    assert!(audio.stretched_peak().unwrap() < 0.1);

    wait_for_loud_window(&mut audio, &project, loud, Duration::from_millis(1500));
}

/// A window stretched before its audio was decoded is silent: it is
/// stretched again once the audio is there, not played out.
#[test]
fn a_fast_forward_started_before_the_audio_is_decoded_plays_it_once_decoded() {
    if vv_audio::stretch::skip_without_rubberband() {
        return;
    }
    let mut project = vv_core::Project::default();
    let timeline = tone_timeline(&mut project, "late.wav", 0.8);
    let mut audio = TimelineAudio::new();
    audio.sync(&project, timeline, 1);
    audio.play();
    audio.request_speed(2.0);
    wait_for_speed(&mut audio, 2.0);
    wait_for_loud_window(&mut audio, &project, timeline, Duration::from_millis(1500));
}

/// `timeline` decoded, playing at 2x.
fn fast_forward_on(project: &vv_core::Project, timeline: TimelineId) -> TimelineAudio {
    let mut audio = TimelineAudio::new();
    while {
        audio.sync(project, timeline, 1);
        audio.has_pending_buffers()
    } {
        std::thread::sleep(Duration::from_millis(10));
    }
    audio.play();
    audio.request_speed(2.0);
    wait_for_speed(&mut audio, 2.0);
    audio
}

/// A drag changes the project every frame.
#[test]
fn a_drag_during_a_fast_forward_stretches_one_window_at_a_time() {
    if vv_audio::stretch::skip_without_rubberband() {
        return;
    }
    let mut project = vv_core::Project::default();
    let timeline = tone_timeline(&mut project, "drag.wav", 0.8);
    let mut audio = fast_forward_on(&project, timeline);
    let before = audio.next_request_id;
    for generation in 2..32 {
        audio.sync(&project, timeline, generation);
        audio.tick();
    }
    assert!(
        audio.next_request_id - before <= 1,
        "{}",
        audio.next_request_id - before
    );

    wait_for_loud_window(&mut audio, &project, timeline, Duration::from_millis(1500));
    assert!(
        audio.next_request_id - before <= 4,
        "{}",
        audio.next_request_id - before
    );
}

#[test]
fn an_edit_while_paused_in_fast_forward_stretches_on_resume() {
    if vv_audio::stretch::skip_without_rubberband() {
        return;
    }
    let mut project = vv_core::Project::default();
    let timeline = tone_timeline(&mut project, "paused.wav", 0.8);
    let mut audio = fast_forward_on(&project, timeline);
    audio.pause();
    while audio.stretch_request.is_some() {
        std::thread::sleep(Duration::from_millis(5));
        audio.tick();
    }
    let before = audio.next_request_id;
    for generation in 2..12 {
        audio.sync(&project, timeline, generation);
        audio.tick();
    }
    assert_eq!(audio.next_request_id, before, "nothing plays");

    audio.play();
    audio.tick();
    assert_eq!(audio.next_request_id, before + 1);
}

#[test]
fn a_failed_stretch_is_not_retried_every_frame() {
    if vv_audio::stretch::skip_without_rubberband() {
        return;
    }
    let mut project = vv_core::Project::default();
    let timeline = tone_timeline(&mut project, "failed.wav", 0.8);
    let mut audio = fast_forward_on(&project, timeline);
    while audio.stretch_request.is_some() {
        std::thread::sleep(Duration::from_millis(5));
        audio.tick();
    }
    let failed = StretchRequest {
        id: audio.next_request_id,
        tempo: 2,
        start: audio.position_sample(),
        frames: FIRST_WINDOW_FRAMES,
        new_window: false,
        complete: false,
    };
    audio.next_request_id += 1;
    audio.stretch_request = Some(failed);
    audio.window_complete = false;
    audio
        .stretch_tx
        .send(StretchResult {
            id: failed.id,
            _mix: audio.mix.clone(),
            samples: Err("failed".into()),
        })
        .unwrap();
    let before = audio.next_request_id;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(5));
        audio.tick();
    }
    assert_eq!(audio.next_request_id, before);
}

fn stretch_result(mix: &Arc<MixSnapshot>) -> StretchResult {
    StretchResult {
        id: 0,
        _mix: mix.clone(),
        samples: Ok(Vec::new()),
    }
}

#[test]
fn a_stretch_result_without_receiver_is_leaked_not_dropped() {
    let mix = Arc::new(MixSnapshot::empty(PROJECT_SAMPLE_RATE, 2));
    let (tx, rx) = mpsc::channel();
    drop(rx);
    hand_back(&tx, stretch_result(&mix));
    assert_eq!(
        Arc::strong_count(&mix),
        2,
        "the snapshot must not be dropped"
    );
}

#[test]
fn a_stretch_result_with_receiver_is_delivered() {
    let mix = Arc::new(MixSnapshot::empty(PROJECT_SAMPLE_RATE, 2));
    let (tx, rx) = mpsc::channel();
    hand_back(&tx, stretch_result(&mix));
    drop(rx.try_recv().expect("delivered"));
    assert_eq!(Arc::strong_count(&mix), 1);
}

fn after_a_rescan(rescan: impl Fn(&mut TimelineAudio)) {
    let mut audio = TimelineAudio::new();
    audio.synced = Some((<TimelineId as vv_core::Id>::from_raw(1), 3));
    rescan(&mut audio);
    assert_eq!(
        audio.synced, None,
        "the next sync rebuilds the timeline mix"
    );

    let media = PathBuf::from("/preview.wav");
    audio.synced_media = Some(media.clone());
    rescan(&mut audio);
    assert_eq!(
        audio.synced_media,
        Some(media),
        "the plugin-free media mix is not rebuilt"
    );
}

#[test]
fn a_rescan_forces_a_timeline_rebuild_and_keeps_a_media_preview() {
    after_a_rescan(TimelineAudio::plugins_rescan_unchanged);
    after_a_rescan(|audio| {
        audio.plugins_rescanned();
    });
}
