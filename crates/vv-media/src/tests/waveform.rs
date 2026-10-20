use super::*;

/// Generates a real x264 file with AAC audio (a sine) via the ffmpeg CLI —
/// the primary use case (compressed x264 + audio), not a mock.
fn make_test_clip(
    file_name: &str,
    duration_secs: u32,
) -> (crate::test_support::TempDir, std::path::PathBuf) {
    let dir = crate::test_support::TempDir::new("vv-media-waveform-test");
    let path = dir.join(file_name);
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size=320x240:rate=25:duration={duration_secs}"),
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=440:sample_rate=48000:duration={duration_secs}"),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ],
        &path,
    );
    (dir, path)
}

#[test]
fn recommended_num_peaks_scales_with_duration_and_is_clamped() {
    assert_eq!(recommended_num_peaks(0.5), MIN_PEAKS);
    assert_eq!(recommended_num_peaks(60.0), 6000);
    assert_eq!(recommended_num_peaks(3600.0), MAX_PEAKS);
}

#[test]
fn peaks_file_round_trips() {
    let peaks: Vec<f32> = (0..100).map(|i| (i as f32) / 99.0).collect();
    let bytes = {
        let mut b = Vec::new();
        b.extend_from_slice(PEAKS_MAGIC);
        b.extend_from_slice(&PEAKS_VERSION.to_le_bytes());
        b.extend_from_slice(&(peaks.len() as u32).to_le_bytes());
        b.extend_from_slice(&5.5f64.to_le_bytes());
        for p in &peaks {
            b.extend_from_slice(&p.to_le_bytes());
        }
        b
    };
    let loaded = read_peaks_file(&bytes).expect("read failed");
    assert_eq!(loaded.peaks, peaks);
    assert!((loaded.audio_duration_secs - 5.5).abs() < 1e-9);
}

#[test]
fn peaks_file_rejects_bad_magic_and_version() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"XXXX");
    bytes.extend_from_slice(&PEAKS_VERSION.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&1.0f64.to_le_bytes());
    bytes.extend_from_slice(&0.5f32.to_le_bytes());
    assert!(read_peaks_file(&bytes).is_none());

    let mut bytes = Vec::new();
    bytes.extend_from_slice(PEAKS_MAGIC);
    bytes.extend_from_slice(&99u32.to_le_bytes()); // unknown version
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&1.0f64.to_le_bytes());
    bytes.extend_from_slice(&0.5f32.to_le_bytes());
    assert!(read_peaks_file(&bytes).is_none());
}

#[test]
fn generate_waveform_produces_normalized_peaks_and_caches_to_disk() {
    let (_dir, path) = make_test_clip("source.mp4", 2);
    let content_hash = 0xCAFEBABE;
    let _ = std::fs::remove_file(waveform_path_for(content_hash, 0));

    assert!(!waveform_exists(content_hash, 0));
    let wf = generate_waveform(&path, content_hash, 0, 500)
        .expect("waveform generation failed")
        .expect("audio expected");
    assert_eq!(wf.peaks.len(), 500);
    assert!(waveform_exists(content_hash, 0));

    // A 440Hz sine is not silent: the global (normalized) peak must
    // reach 1.0 and the values must stay in [0,1].
    let peak = wf.peaks.iter().cloned().fold(0.0_f32, f32::max);
    assert!(
        (peak - 1.0).abs() < 1e-6,
        "peak={peak}, expected 1.0 after normalization"
    );
    assert!(wf.peaks.iter().all(|p| (0.0..=1.0).contains(p)));

    // The audio duration must be ~2s (the file is 2s long).
    assert!(
        (wf.audio_duration_secs - 2.0).abs() < 0.2,
        "dur={:?}",
        wf.audio_duration_secs
    );

    // The cache file, read back, returns the same peaks.
    let reloaded = load_waveform(content_hash, 0).expect("reload failed");
    assert_eq!(reloaded.peaks, wf.peaks);
    assert!((reloaded.audio_duration_secs - wf.audio_duration_secs).abs() < 1e-9);
}

#[test]
fn stereo_audio_peaks_are_not_compressed_into_the_first_half_of_bins() {
    // 4s stereo: silence in the first 2s, tone in the second 2s. With the
    // bug (global_sample advanced once per *interleaved sample* instead
    // of once per *frame*, hence twice as fast for stereo) the whole
    // waveform ended up squeezed/clamped into the first half of the bins,
    // and the tone (which starts at the middle of the real duration)
    // appeared already at 3/4 of the width instead of in the last
    // quarter — "the waveform runs ahead of the sound".
    let dir = crate::test_support::TempDir::new("vv-media-waveform-test");
    let path = dir.join("stereo_half_silent.mp4");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=exprs='if(lt(t,2),0,sin(2*PI*440*t))':s=48000:d=4:c=stereo",
            "-c:a",
            "aac",
        ],
        &path,
    );

    let content_hash = 0x51DE7357;
    let _ = std::fs::remove_file(waveform_path_for(content_hash, 0));
    let wf = generate_waveform(&path, content_hash, 0, 400)
        .expect("waveform generation failed")
        .expect("audio expected");

    // The tone starts exactly at the middle of the duration (2s of 4s): with
    // 400 bins, the bin where it comes out of the silence must be near bin
    // 200 (half the width). With the bug, the clamp at `num_peaks - 1` hid
    // a check too weak on "only the first/last portion" (the tone,
    // doubled in speed, still ended up crushed into the very last bin
    // rather than disappearing) — here we check the *position* of the
    // transition, not just that the tone shows up somewhere.
    let first_loud_bin = wf
        .peaks
        .iter()
        .position(|&p| p > 0.3)
        .expect("the tone should exceed the threshold somewhere");
    assert!(
        (170..230).contains(&first_loud_bin),
        "the tone starts at bin {first_loud_bin} (expected near bin 200, half of the 400 total bins for a transition halfway through the 4s)"
    );
}

/// The same silence->tone transition as the stereo test, but on 6 channels
/// (5.1 side, the exact layout of the real file that exposed the stereo
/// channel bug — bbb_sunflower with a 5.1 AC-3 track): checks that the
/// `chunks_exact(channels)` fix holds for channels > 2 too, not just
/// the 2-channel case already covered.
/// With a non-integer number of samples per bin (the normal case: the
/// bins come from the video duration) the position of the peaks must
/// not slide along the file.
#[test]
fn peaks_stay_aligned_when_samples_per_bin_is_fractional() {
    let dir = crate::test_support::TempDir::new("vv-media-waveform-test");
    let path = dir.join("tone_at_8s.wav");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=exprs='if(lt(t,8),0,sin(2*PI*440*t))':s=48000:d=10",
        ],
        &path,
    );

    let content_hash = 0xF4AC7;
    let _ = std::fs::remove_file(waveform_path_for(content_hash, 0));
    // 480000 samples / 250000 bins = 1.92 samples per bin.
    let num_peaks = 250_000;
    let wf = generate_waveform(&path, content_hash, 0, num_peaks)
        .expect("waveform generation failed")
        .expect("audio expected");
    let first_loud_bin = wf
        .peaks
        .iter()
        .position(|&p| p > 0.3)
        .expect("tone expected");
    let expected = num_peaks * 8 / 10;
    assert!(
        // The sine starts at 0: it crosses the threshold a few samples later.
        first_loud_bin.abs_diff(expected) <= 5,
        "tone at bin {first_loud_bin}, expected {expected} (8 s out of 10)"
    );
    let _ = std::fs::remove_file(waveform_path_for(content_hash, 0));
}

#[test]
fn six_channel_audio_peaks_are_not_compressed_into_the_first_half_of_bins() {
    let dir = crate::test_support::TempDir::new("vv-media-waveform-test");
    let path = dir.join("six_channel_half_silent.mp4");
    let tone = "if(lt(t,2),0,sin(2*PI*440*t))";
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            &format!(
                "aevalsrc=exprs='{tone}|{tone}|{tone}|{tone}|{tone}|{tone}':s=48000:d=4:c=5.1"
            ),
            "-c:a",
            "ac3",
        ],
        &path,
    );

    let content_hash = 0x51DE7357_6C6C6C6C;
    let _ = std::fs::remove_file(waveform_path_for(content_hash, 0));
    let wf = generate_waveform(&path, content_hash, 0, 400)
        .expect("waveform generation failed")
        .expect("audio expected");

    let first_loud_bin = wf
        .peaks
        .iter()
        .position(|&p| p > 0.3)
        .expect("the tone should exceed the threshold somewhere");
    assert!(
        (170..230).contains(&first_loud_bin),
        "the tone starts at bin {first_loud_bin} (expected near bin 200, half of the 400 total bins for a transition halfway through the 4s)"
    );
}

#[test]
fn generate_waveform_returns_none_for_a_video_without_audio() {
    let dir = crate::test_support::TempDir::new("vv-media-waveform-test");
    let path = dir.join("silent.mp4");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );

    let result = generate_waveform(&path, 0xDEADBEEF, 0, 200).expect("generation failed");
    assert!(result.is_none(), "no audio track: no peaks");
}

/// A file with two audio streams (see `audio::decode_audio_track_selects_the_requested_stream_index_not_just_the_best`
/// for the same fixture): the waveform generated for stream 1 must be
/// the one of the *second* signal, not always fall back to the first.
#[test]
fn generate_waveform_selects_the_requested_stream_index() {
    let dir = crate::test_support::TempDir::new("vv-media-waveform-test");
    let path = dir.join("two_streams.mp4");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=48000:duration=1",
            "-map",
            "0:a",
            "-map",
            "1:a",
            "-c:a",
            "aac",
        ],
        &path,
    );

    let content_hash = 0x57EA2001;
    let _ = std::fs::remove_file(waveform_path_for(content_hash, 0));
    let _ = std::fs::remove_file(waveform_path_for(content_hash, 1));

    let first = generate_waveform(&path, content_hash, 0, 200)
        .unwrap()
        .expect("stream 0 expected");
    assert!((first.audio_duration_secs - 1.0).abs() < 0.2);

    let second = generate_waveform(&path, content_hash, 1, 200)
        .unwrap()
        .expect("stream 1 expected");
    assert!((second.audio_duration_secs - 1.0).abs() < 0.2);

    assert!(
        generate_waveform(&path, content_hash, 2, 200)
            .unwrap()
            .is_none(),
        "no audio stream at index 2"
    );
}
