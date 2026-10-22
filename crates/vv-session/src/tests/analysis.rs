use super::*;
use crate::test_support::TempDir;
use vv_core::{Clip, ClipSource, MediaItem, MediaMeta, Timeline, Track, TrackKind};

/// 1 s of silence, then 1 s of a sine at 440 Hz (peak 0.5).
fn silence_then_tone(dir: &TempDir, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=48000:cl=mono:d=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
            "-filter_complex",
            "[1]volume=4[tone];[0][tone]concat=n=2:v=0:a=1",
        ],
        &path,
    );
    path
}

#[test]
fn media_levels_tell_silence_from_sound_window_by_window() {
    let dir = TempDir::new("vv-session-analysis");
    let path = silence_then_tone(&dir, "media.wav");
    let levels = media_audio_levels(&path, 0, Rational::new(25, 1), 0..60, 25).unwrap();

    assert_eq!(levels.len(), 3, "the last window is the 10 frames left");
    assert_eq!(levels[0].rms_db, FLOOR_DB);
    assert!(levels[1].rms_db > -12.0, "{:?}", levels[1]);
    assert!((levels[1].peak_db - -6.0).abs() < 1.0, "{:?}", levels[1]);
    assert_eq!(levels[2].rms_db, FLOOR_DB, "past the end of the file");
}

#[test]
fn media_levels_of_a_missing_stream_are_an_error() {
    let dir = TempDir::new("vv-session-analysis");
    let path = silence_then_tone(&dir, "missing-stream.wav");
    let error = media_audio_levels(&path, 3, Rational::new(25, 1), 0..25, 25).unwrap_err();
    assert_eq!(error, "the media has no audio stream 3");
}

#[test]
fn timeline_levels_follow_the_clip_position() {
    let dir = TempDir::new("vv-session-analysis");
    let path = silence_then_tone(&dir, "timeline.wav");
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 50,
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
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let mut track = Track::new(TrackKind::Audio);
    // Only the tone, from timeline frame 50.
    track.clips.push(Clip::from_source_range(
        project.alloc_clip_id(),
        ClipSource::Media(media),
        25,
        50,
        50,
        Rational::one(),
    ));
    let timeline = project.timelines.insert(Timeline {
        name: "t".into(),
        fps: Rational::new(25, 1),
        resolution: (64, 48),
        tracks: vec![track],
        markers: Vec::new(),
        master: Default::default(),
    });

    let levels = timeline_audio_levels(&project, timeline, 0..75, 25).unwrap();

    let loud: Vec<bool> = levels.iter().map(|l| l.rms_db > -20.0).collect();
    assert_eq!(loud, [false, false, true]);
}
