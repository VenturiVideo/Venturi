use super::*;

/// Android screen capture declares `r_frame_rate = 90000`, the
/// container tick rate: believing it inflated `duration_frames` by five
/// orders of magnitude and froze the preview.
#[test]
fn a_tick_rate_masquerading_as_r_frame_rate_is_not_plausible() {
    assert_eq!(plausible_fps(90000, 1), None);
    assert_eq!(plausible_fps(0, 0), None);
    assert_eq!(plausible_fps(60, 1), Some(Rational::new(60, 1)));
    assert_eq!(plausible_fps(30000, 1001), Some(Rational::new(30000, 1001)));
}

/// A clip that is still for most of its length but holds real motion in
/// bursts: the average (~4 fps here) would make those stretches play in
/// slideshow, the peak keeps them smooth.
#[test]
fn the_measured_rate_is_the_peak_not_the_average() {
    let mut times: Vec<f64> = (0..60).map(|i| f64::from(i) / 60.0).collect();
    times.extend((1..10).map(f64::from));
    assert_eq!(peak_fps_of_window(&times), Some(Rational::new(60, 1)));
}

#[test]
fn a_rate_measured_off_by_jitter_snaps_to_the_standard_one() {
    assert_eq!(snapped_to_a_common_rate(61), 60);
    assert_eq!(snapped_to_a_common_rate(55), 60);
    assert_eq!(snapped_to_a_common_rate(31), 30);
    // Too far from any standard rate: it is taken as it is.
    assert_eq!(snapped_to_a_common_rate(40), 40);
}

#[test]
fn the_measured_rate_is_capped() {
    let times: Vec<f64> = (0..500).map(|i| f64::from(i) / 500.0).collect();
    assert_eq!(
        peak_fps_of_window(&times),
        Some(Rational::new(MAX_MEASURED_FPS, 1))
    );
}

#[test]
fn too_few_packets_to_measure_a_rate() {
    assert_eq!(peak_fps_of_window(&[]), None);
    assert_eq!(peak_fps_of_window(&[0.0]), None);
}

/// Reproduces the bug scenario: a container declaring more frames than
/// the decoder actually produces (simulated here by passing
/// `verify_last_decodable_frame` a `nominal` inflated past the true
/// count, instead of a real broken container). It must correct it to
/// the true last decodable frame, not trust the given value.
#[test]
fn verify_last_decodable_frame_corrects_an_inflated_nominal() {
    let dir = crate::test_support::TempDir::new("vv-media-probe-test");
    let path = dir.join("dieci_frame.mp4");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=10:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );

    let fps = Rational::new(10, 1);
    let real = verify_last_decodable_frame(&path, fps, 10);
    let corrected = verify_last_decodable_frame(&path, fps, 10_000);

    assert_eq!(
        corrected, real,
        "an inflated nominal count must converge to the real last decodable frame"
    );
    assert!(
        corrected <= 15,
        "10 real frames at 10fps: the corrected count must not stay near the inflated nominal ({corrected})"
    );
}

/// Real case of the overestimate: the audio lasts longer than the video, so
/// the container duration (the maximum across the streams) multiplied by
/// the fps promises many more frames than the video has.
#[test]
fn probe_ignores_frames_promised_by_an_audio_track_longer_than_the_video() {
    let dir = crate::test_support::TempDir::new("vv-media-probe-test");
    let path = dir.join("audio_piu_lungo.mp4");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=4",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ],
        &path,
    );

    let meta = probe(&path).expect("probe failed");
    assert!(
        (meta.duration_frames - 25).abs() <= 2,
        "1 s of video at 25 fps, not the frames promised by 4 s of audio: {}",
        meta.duration_frames
    );
}

/// Generates a real x264 file (with AAC audio) via the ffmpeg CLI and checks
/// that the probe reads coherent metadata. This is the primary use case
/// stated by the user (compressed x264), not a mock.
#[test]
fn probe_reads_x264_metadata() {
    let dir = crate::test_support::TempDir::new("vv-media-probe-test");
    let path = dir.join("sample.mp4");

    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=640x360:rate=25:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ],
        &path,
    );

    let meta = probe(&path).expect("probe failed");
    assert_eq!(meta.width, 640);
    assert_eq!(meta.height, 360);
    assert_eq!(meta.fps, Rational::new(25, 1));
    assert!(meta.has_audio);
    assert_eq!(meta.sample_rate, 48000);
    assert_eq!(meta.audio_streams, 1);
    // ~2s at 25fps: tolerates a few rounding frames on the container.
    assert!((meta.duration_frames - 50).abs() <= 2);
}

#[test]
fn probe_accepts_an_audio_only_file() {
    let dir = crate::test_support::TempDir::new("vv-media-probe-test");
    let path = dir.join("tone.wav");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=2",
        ],
        &path,
    );

    let meta = probe(&path).expect("probe failed");
    assert!(!meta.has_video);
    assert!(meta.has_audio);
    assert_eq!((meta.width, meta.height), (0, 0));
    assert_eq!(meta.fps, AUDIO_ONLY_FPS);
    assert_eq!(meta.sample_rate, 44_100);
    assert_eq!(meta.duration_frames, 60, "2 s at nominal fps");
}

#[test]
fn probe_takes_an_audio_file_with_cover_art_for_audio_only() {
    let dir = crate::test_support::TempDir::new("vv-media-probe-test");
    let cover = dir.join("cover.jpg");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:size=64x64",
            "-frames:v",
            "1",
            "-update",
            "1",
        ],
        &cover,
    );
    let path = dir.join("tone-with-cover.flac");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=2",
            "-i",
            cover.to_str().unwrap(),
            "-map",
            "0",
            "-map",
            "1",
            "-c:v",
            "copy",
            "-disposition:v",
            "attached_pic",
        ],
        &path,
    );

    let meta = probe(&path).expect("probe failed");
    assert!(!meta.has_video);
    assert!(meta.has_audio);
    assert_eq!(meta.fps, AUDIO_ONLY_FPS);
    assert_eq!(meta.duration_frames, 60, "2 s at nominal fps");
    assert_eq!(meta.file.video_codec, None);
}

#[test]
fn probe_image_reads_dimensions_and_reports_the_image_sentinel() {
    let dir = crate::test_support::TempDir::new("vv-media-probe-test");
    let path = dir.join("still.png");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=640x360:rate=1:duration=1",
            "-frames:v",
            "1",
            "-update",
            "1",
        ],
        &path,
    );

    let meta = probe_image(&path).expect("probe_image failed");
    assert_eq!((meta.width, meta.height), (640, 360));
    assert_eq!(meta.fps, IMAGE_FPS);
    assert!(meta.has_video);
    assert!(!meta.has_audio);
    assert_eq!(meta.duration_frames, vv_core::IMAGE_DURATION_FRAMES);
    assert!(meta.is_image());
}

/// Every import path (media pool, drop, OTIO) goes through it: an image
/// probed as a one-frame video would last a single frame on the timeline.
#[test]
fn probe_media_recognizes_an_image_by_its_extension() {
    let dir = crate::test_support::TempDir::new("vv-media-probe-test");
    let path = dir.join("STILL.PNG");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:size=64x48:rate=1:duration=1",
            "-frames:v",
            "1",
            "-update",
            "1",
        ],
        &path,
    );
    assert!(probe_media(&path).expect("probe_media failed").is_image());
}

#[test]
fn probe_rejects_a_file_without_audio_or_video() {
    let dir = crate::test_support::TempDir::new("vv-media-probe-test");
    let path = dir.join("subtitles.srt");
    std::fs::write(&path, "1\n00:00:00,000 --> 00:00:01,000\nhello\n").unwrap();
    assert!(probe(&path).is_err());
}

#[test]
fn content_fingerprint_is_stable_for_the_same_unchanged_file() {
    let dir = crate::test_support::TempDir::new("vv-media-fingerprint-test");
    let path = dir.join("stable.bin");
    std::fs::write(&path, "test content").unwrap();

    let a = content_fingerprint(&path).unwrap();
    let b = content_fingerprint(&path).unwrap();
    assert_eq!(a, b);
}

#[test]
fn content_fingerprint_differs_for_different_sized_files() {
    let dir = crate::test_support::TempDir::new("vv-media-fingerprint-test");
    let path_a = dir.join("a.bin");
    let path_b = dir.join("b.bin");
    std::fs::write(&path_a, "short content").unwrap();
    std::fs::write(&path_b, "content much longer than the short one").unwrap();

    assert_ne!(
        content_fingerprint(&path_a).unwrap(),
        content_fingerprint(&path_b).unwrap()
    );
}

#[test]
fn content_fingerprint_changes_when_the_file_is_rewritten_with_different_content() {
    let dir = crate::test_support::TempDir::new("vv-media-fingerprint-test");
    let path = dir.join("rewritten.bin");

    std::fs::write(&path, "first version").unwrap();
    let before = content_fingerprint(&path).unwrap();

    // A different size guarantees the fingerprint changes even
    // if the filesystem has an mtime resolution too coarse
    // to record the write as "later" than the previous one
    // within the duration of the test.
    std::fs::write(&path, "second version, longer than the first").unwrap();
    let after = content_fingerprint(&path).unwrap();

    assert_ne!(before, after);
}
