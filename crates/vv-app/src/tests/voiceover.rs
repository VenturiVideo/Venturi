use super::*;
use vv_core::Track;

fn temp_dir(name: &str) -> vv_media::test_support::TempDir {
    vv_media::test_support::TempDir::new(&format!("vv-voiceover-{name}"))
}

/// A timeline at 25 fps with a video track and an armed audio track holding
/// a 4 s clip.
fn app_with_armed_track() -> (VenturiApp, TimelineId) {
    let mut app = VenturiApp::default();
    let mut audio = Track::new(TrackKind::Audio);
    audio.armed = true;
    audio.insert_sorted(Clip::from_source_range(
        app.session.project.alloc_clip_id(),
        ClipSource::SolidColor,
        0,
        100,
        0,
        Rational::one(),
    ));
    let timeline = app.session.project.timelines.insert(vv_core::Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (320, 240),
        tracks: vec![Track::new(TrackKind::Video), audio],
        markers: Vec::new(),
        master: Default::default(),
    });
    app.timeline_id = Some(timeline);
    (app, timeline)
}

fn one_second_take() -> Take {
    Take {
        samples: (0..48_000).map(|i| (i as f32 * 0.03).sin() * 0.3).collect(),
        sample_rate: 48_000,
        channels: 1,
    }
}

#[test]
fn takes_are_numbered_past_the_ones_already_there_in_any_format() {
    let dir = temp_dir("names");
    let mp3 = AudioFileFormat::Mp3;
    assert_eq!(next_take_path(&dir, mp3), dir.join("Voiceover 001.mp3"));
    std::fs::write(dir.join("Voiceover 001.wav"), b"").unwrap();
    std::fs::write(dir.join("Voiceover 002.mp3"), b"").unwrap();
    std::fs::write(dir.join("Voiceover 004.flac"), b"").unwrap();
    assert_eq!(
        next_take_path(&dir, mp3),
        dir.join("Voiceover 003.mp3"),
        "001 is taken by a WAV"
    );
}

#[test]
fn an_armed_track_waits_for_a_saved_project() {
    let dir = temp_dir("guard");
    let (mut app, _) = app_with_armed_track();
    assert!(!app.prepare_take());
    assert!(app.timeline_state.record_needs_save);
    app.session.set_path(Some(dir.join("project.vvproj")));
    app.timeline_state.record_needs_save = false;
    assert!(app.prepare_take());
    assert!(!app.timeline_state.record_needs_save);

    app.session.set_path(None);
    app.settings.recording_dir = Some(dir.to_path_buf());
    assert!(
        app.prepare_take(),
        "a folder of its own needs no saved project"
    );
    app.settings.recording_dir = None;
    let audio = 1;
    app.session.project.timelines[app.timeline_id.unwrap()].tracks[audio].armed = false;
    assert!(app.prepare_take(), "nothing armed, nothing to save first");
}

#[test]
fn a_take_is_saved_next_to_the_project_and_overwrites_the_track_in_one_step() {
    let dir = temp_dir("place");
    let (mut app, timeline) = app_with_armed_track();
    app.session.set_path(Some(dir.join("project.vvproj")));
    let steps_before = app.session.history.labels().count();

    app.place_take(timeline, 25, &[1], &one_second_take())
        .unwrap();

    let format = AudioFileFormat::resolve(None);
    let file = format!("Voiceover 001.{}", format.extension());
    let take_file = dir.join(RECORDINGS_DIR).join(file);
    assert!(take_file.exists());
    let spans = |app: &VenturiApp| -> Vec<(FrameIdx, FrameIdx, bool)> {
        app.session.project.timelines[timeline].tracks[1]
            .clips
            .iter()
            .map(|c| {
                let recorded = matches!(c.source, ClipSource::Media(_));
                (
                    c.timeline_start,
                    c.timeline_start + c.timeline_len,
                    recorded,
                )
            })
            .collect()
    };
    assert_eq!(
        spans(&app),
        [(0, 25, false), (25, 50, true), (50, 100, false)],
        "the second of the take replaced what was under it"
    );
    assert!(
        !app.session.project.timelines[timeline].tracks[1].armed,
        "disarmed after the take"
    );
    assert_eq!(app.session.history.labels().count(), steps_before + 1);
    assert_eq!(
        app.session.history.labels().last(),
        Some(CommandLabel::RecordVoiceover)
    );

    app.session.history.undo(&mut app.session.project);
    assert_eq!(spans(&app), [(0, 100, false)]);
    assert!(app.session.project.timelines[timeline].tracks[1].armed);
    assert!(
        app.session
            .project
            .media_pool
            .values()
            .all(|m| m.path != take_file)
    );
}

#[test]
fn voiceover_length_comes_from_recorded_samples_for_every_available_format() {
    for format in AudioFileFormat::available() {
        let (mut app, timeline) = app_with_armed_track();
        let dir = temp_dir(format.id());
        app.session.set_path(Some(dir.join("project.vvproj")));
        app.settings.recording_format = Some(format);
        app.place_take(timeline, 25, &[1], &one_second_take())
            .unwrap();
        let recorded = app.session.project.timelines[timeline].tracks[1]
            .clips
            .iter()
            .find(|clip| matches!(clip.source, ClipSource::Media(_)))
            .unwrap();
        assert_eq!(recorded.timeline_len, 25, "{format:?}");
        let ClipSource::Media(media_id) = recorded.source else {
            unreachable!()
        };
        let meta = &app.session.project.media_pool[media_id].meta;
        assert_eq!(meta.duration_frames as f64, meta.fps.as_f64(), "{format:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn take_duration_is_rounded_once_at_the_timeline_frame_rate() {
    for fps in [
        Rational::new(24, 1),
        Rational::new(25, 1),
        Rational::new(30_000, 1001),
    ] {
        for samples in [48_001, 48_912, 49_008] {
            let (mut app, timeline) = app_with_armed_track();
            app.session.project.timelines[timeline].fps = fps;
            let dir = temp_dir("rounding");
            app.session.set_path(Some(dir.join("project.vvproj")));
            app.settings.recording_format = Some(AudioFileFormat::Wav);
            let mut take = one_second_take();
            take.samples.resize(samples, 0.0);
            app.place_take(timeline, 0, &[1], &take).unwrap();
            let recorded = app.session.project.timelines[timeline].tracks[1]
                .clips
                .iter()
                .find(|clip| matches!(clip.source, ClipSource::Media(_)))
                .unwrap();
            let expected = (take.seconds() * fps.as_f64()).round() as FrameIdx;
            assert_eq!(
                recorded.timeline_len, expected,
                "{fps:?}: {samples} samples"
            );
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[test]
fn recorded_media_keeps_the_tail_beyond_its_rounded_timeline_clip() {
    let (mut app, timeline) = app_with_armed_track();
    app.session.project.timelines[timeline].fps = Rational::new(24, 1);
    let dir = temp_dir("media-tail");
    app.session.set_path(Some(dir.join("project.vvproj")));
    app.settings.recording_format = Some(AudioFileFormat::Wav);
    let mut take = one_second_take();
    take.samples.resize(48_912, 0.25);
    app.place_take(timeline, 0, &[1], &take).unwrap();
    let recorded = app.session.project.timelines[timeline].tracks[1]
        .clips
        .iter()
        .find(|clip| matches!(clip.source, ClipSource::Media(_)))
        .unwrap();
    assert_eq!(recorded.timeline_len, 24);
    let ClipSource::Media(id) = recorded.source else {
        unreachable!()
    };
    let meta = &app.session.project.media_pool[id].meta;
    assert!(
        meta.duration_frames as f64 / meta.fps.as_f64() >= take.seconds(),
        "reusing the media must retain the recorded tail"
    );
    assert!((meta.duration_frames - 1) as f64 / meta.fps.as_f64() < take.seconds());
    let media = &app.session.project.media_pool[id];
    let decoded = vv_media::decode_audio_track(&media.path, 0)
        .unwrap()
        .unwrap();
    let tail_start = decoded.sample_rate as usize * decoded.channels as usize;
    let tail = decoded.samples.get(tail_start..).unwrap();
    assert_eq!(tail.len(), 912, "the complete recorded tail must remain");
    assert!(
        tail.iter().all(|sample| (*sample - 0.25).abs() < 1e-4),
        "the recorded tail must retain its audio samples"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_subframe_take_creates_one_frame_instead_of_a_zero_length_clip() {
    let (mut app, timeline) = app_with_armed_track();
    let dir = temp_dir("subframe");
    app.session.set_path(Some(dir.join("project.vvproj")));
    app.settings.recording_format = Some(AudioFileFormat::Wav);
    let mut take = one_second_take();
    take.samples.truncate(1);
    app.place_take(timeline, 0, &[1], &take).unwrap();
    let recorded = app.session.project.timelines[timeline].tracks[1]
        .clips
        .iter()
        .find(|clip| matches!(clip.source, ClipSource::Media(_)))
        .unwrap();
    assert_eq!(recorded.timeline_len, 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_live_waveform_grows_by_whole_blocks_as_the_take_arrives() {
    let mut take = Take {
        samples: Vec::new(),
        sample_rate: 1000,
        channels: 2,
    };
    let (mut peaks, mut done) = (Vec::new(), 0);
    // 25 frames: two blocks of 10 and 5 frames still waiting.
    take.samples = (0..25)
        .flat_map(|i| [i as f32 / 100.0, -0.9 * (i == 3) as i32 as f32])
        .collect();
    extend_peaks(&mut peaks, &mut done, &take);
    assert_eq!(peaks, [0.9, 0.19]);
    assert_eq!(done, 20);

    take.samples.extend([0.5, 0.0].repeat(5));
    extend_peaks(&mut peaks, &mut done, &take);
    assert_eq!(peaks, [0.9, 0.19, 0.5]);
    assert_eq!(done, 30);
}
