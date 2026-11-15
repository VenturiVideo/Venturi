use super::*;

struct Fixture(std::path::PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One second of a 440 Hz tone, stereo, at `rate`.
fn tone(rate: u32) -> Vec<f32> {
    (0..rate)
        .flat_map(|i| {
            let s = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin() * 0.5;
            [s, s]
        })
        .collect()
}

#[test]
fn every_available_format_writes_a_file_that_reads_back_at_its_length() {
    let dir = crate::test_support::TempDir::new("vv-audio-file");
    let available = AudioFileFormat::available();
    assert!(available.contains(&AudioFileFormat::Wav));
    for format in available {
        // 44.1 kHz: Opus has to resample to 48 kHz.
        let path = dir.join(format!("take.{}", format.extension()));
        write_audio_file(&path, format, &tone(44_100), 44_100, 2).unwrap();
        let meta = crate::probe_media(&path).unwrap();
        assert!(meta.has_audio && !meta.has_video, "{format:?}");
        let decoded = crate::decode_audio_track(&path, 0).unwrap().unwrap();
        let frames = decoded.samples.len() as f64 / decoded.channels as f64;
        let secs = frames / decoded.sample_rate as f64;
        assert!((secs - 1.0).abs() < 0.08, "{format:?}: {secs} s");
        let peak = decoded.samples.iter().fold(0f32, |p, s| p.max(s.abs()));
        assert!((0.4..0.6).contains(&peak), "{format:?}: peak {peak}");
    }
}

#[test]
fn an_unavailable_choice_falls_back_to_mp3_then_wav() {
    let fallback = if AudioFileFormat::Mp3.is_available() {
        AudioFileFormat::Mp3
    } else {
        AudioFileFormat::Wav
    };
    assert_eq!(AudioFileFormat::resolve(None), fallback);
    assert_eq!(
        AudioFileFormat::resolve(Some(AudioFileFormat::Wav)),
        AudioFileFormat::Wav
    );
    for format in AudioFileFormat::ALL {
        assert_eq!(AudioFileFormat::from_id(format.id()), Some(format));
    }
}

#[test]
fn short_final_audio_frames_preserve_the_recorded_sample_count() {
    let root = std::env::temp_dir().join(format!("vv-audio-tail-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let fixture = Fixture(root);
    for format in [
        AudioFileFormat::Wav,
        AudioFileFormat::Flac,
        AudioFileFormat::Mp3,
        AudioFileFormat::Opus,
    ] {
        if !format.is_available() {
            continue;
        }
        for frames in [1_337, 48_000, 48_001] {
            let samples = &tone(48_000)[..frames.min(48_000) * 2];
            let mut samples = samples.to_vec();
            samples.resize(frames * 2, 0.25);
            let path = fixture.0.join(format!("{frames}.{}", format.extension()));
            write_audio_file(&path, format, &samples, 48_000, 2).unwrap();
            let decoded = crate::decode_audio_track(&path, 0).unwrap().unwrap();
            assert_eq!(decoded.sample_rate, 48_000);
            assert_eq!(
                decoded.samples.len() / decoded.channels as usize,
                frames,
                "{format:?}"
            );
        }
    }
}

#[test]
fn recording_encoders_match_reference_decoded_lengths() {
    let root = std::env::temp_dir().join(format!("vv-audio-reference-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let dir = Fixture(root);
    let cli = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .expect("ffmpeg must be on PATH");
    assert!(cli.status.success(), "could not list FFmpeg CLI encoders");
    let encoders = String::from_utf8(cli.stdout).unwrap();
    let formats: Vec<_> = AudioFileFormat::available()
        .into_iter()
        .filter(|format| {
            let available = encoders
                .lines()
                .any(|line| line.split_whitespace().nth(1) == Some(format.encoder_name()));
            if !available {
                eprintln!("{format:?} reference comparison not run: CLI encoder unavailable");
            }
            available
        })
        .collect();
    assert!(
        formats.contains(&AudioFileFormat::Wav),
        "reference PCM encoder required"
    );
    let mut failures = Vec::new();
    for frames in [1_337, 48_000, 48_001] {
        let mut samples = tone(48_000);
        samples.resize(frames * 2, 0.25);
        let source = dir.0.join(format!("{frames}-source.wav"));
        write_audio_file(&source, AudioFileFormat::Wav, &samples, 48_000, 2).unwrap();
        for &format in &formats {
            let path = dir.0.join(format!("{frames}.{}", format.extension()));
            let reference = dir
                .0
                .join(format!("{frames}-reference.{}", format.extension()));
            write_audio_file(&path, format, &samples, 48_000, 2).unwrap();
            crate::test_support::ffmpeg(
                &[
                    "-i",
                    source.to_str().unwrap(),
                    "-c:a",
                    format.encoder_name(),
                    "-b:a",
                    &format.bit_rate().to_string(),
                ],
                &reference,
            );
            let decoded = crate::decode_audio_track(&path, 0).unwrap().unwrap();
            let expected = crate::decode_audio_track(&reference, 0).unwrap().unwrap();
            assert_eq!(decoded.sample_rate, expected.sample_rate, "{format:?}");
            assert_eq!(decoded.channels, expected.channels, "{format:?}");
            let actual = decoded.samples.len() / decoded.channels as usize;
            let expected = expected.samples.len() / expected.channels as usize;
            if actual != expected {
                failures.push(format!("{format:?}/{frames}: {actual} != {expected}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("; "));
}
