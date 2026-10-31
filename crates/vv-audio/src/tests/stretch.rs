use super::*;

fn frequency(samples: &[f32], rate: u32) -> f64 {
    let samples = &samples[samples.len() / 4..samples.len() * 3 / 4];
    let crossings = samples
        .windows(2)
        .filter(|s| s[0] <= 0.0 && s[1] > 0.0)
        .count();
    crossings as f64 * rate as f64 / samples.len() as f64
}

#[test]
fn pitch_shift_changes_frequency_in_both_directions_without_changing_duration() {
    for (semitones, cents, expected) in [
        (12.0, 0.0, 880.0),
        (-12.0, 0.0, 220.0),
        (0.0, 100.0, 466.16),
    ] {
        let params = vv_core::PitchShift {
            semitones,
            cents,
            ..Default::default()
        };
        let samples = sine(440.0, 48_000, 2.0);
        let mut processor = Stretcher::with_pitch(48_000, 1, 1.0, params.ratio(), false).unwrap();
        let mut out = Vec::new();
        processor.push(&samples, &mut out).unwrap();
        processor.finish(&mut out).unwrap();
        assert!(
            out.len().abs_diff(samples.len()) < 2048,
            "{} samples",
            out.len()
        );
        let heard = frequency(&out, 48_000);
        assert!(
            (heard - expected).abs() < 4.0,
            "expected {expected}, heard {heard}"
        );
    }
}

#[test]
fn stereo_pitch_shift_keeps_channels_aligned_and_is_independent_of_input_chunks() {
    let input: Vec<f32> = sine(440.0, 48_000, 2.0)
        .into_iter()
        .flat_map(|x| [x, x * 0.5])
        .collect();
    let run = |chunk_size| {
        let mut processor = Stretcher::with_pitch(48_000, 2, 1.0, 2.0, false).unwrap();
        let mut out = Vec::new();
        for chunk in input.chunks(chunk_size) {
            processor.push(chunk, &mut out).unwrap();
        }
        processor.finish(&mut out).unwrap();
        out
    };
    let whole = run(input.len());
    let pieces = run(2 * 701);
    assert_eq!(whole, pieces);
    let middle = &whole[whole.len() / 4 / 2 * 2..whole.len() * 3 / 4 / 2 * 2];
    let [mut left_power, mut right_power, mut cross] = [0.0f64; 3];
    for [left, right] in middle.as_chunks::<2>().0 {
        left_power += (*left as f64).powi(2);
        right_power += (*right as f64).powi(2);
        cross += *left as f64 * *right as f64;
    }
    let balance = (right_power / left_power).sqrt();
    let correlation = cross / (left_power * right_power).sqrt();
    assert!(
        (balance - 0.5).abs() < 0.01 && correlation > 0.99,
        "balance {balance}, correlation {correlation}"
    );
    let left: Vec<f32> = whole.as_chunks::<2>().0.iter().map(|[l, _]| *l).collect();
    assert!((frequency(&left, 48_000) - 880.0).abs() < 4.0);
}

#[test]
fn pitch_shift_and_tempo_are_independent() {
    let input = sine(440.0, 48_000, 2.0);
    let mut processor = Stretcher::with_pitch(48_000, 1, 2.0, 2.0, false).unwrap();
    let mut out = Vec::new();
    processor.push(&input, &mut out).unwrap();
    processor.finish(&mut out).unwrap();
    assert!(out.len().abs_diff(48_000) < 2048);
    assert!((frequency(&out, 48_000) - 880.0).abs() < 4.0);
}

#[test]
fn invalid_pitch_and_audio_formats_are_rejected() {
    for pitch in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(Stretcher::with_pitch(48_000, 1, 1.0, pitch, false).is_err());
    }
    assert!(Stretcher::with_pitch(0, 1, 1.0, 1.0, false).is_err());
    assert!(Stretcher::with_pitch(48_000, 0, 1.0, 1.0, false).is_err());
    assert!(Stretcher::new(48_000, 1, f64::INFINITY).is_err());
}

fn sine(freq: f32, sample_rate: u32, secs: f32) -> Vec<f32> {
    let n = (sample_rate as f32 * secs) as usize;
    (0..n)
        .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate as f32).sin())
        .collect()
}

#[test]
fn stretch_at_tempo_2x_roughly_halves_duration() {
    let sample_rate = 48_000;
    let samples = sine(440.0, sample_rate, 2.0); // mono, 2s
    let stretched = stretch_samples(&samples, sample_rate, 1, 2.0).unwrap();

    let original_frames = samples.len();
    let stretched_frames = stretched.len();
    let ratio = original_frames as f64 / stretched_frames as f64;
    assert!(
        (ratio - 2.0).abs() < 0.05,
        "original={original_frames} stretched={stretched_frames} ratio={ratio}"
    );

    let peak = stretched
        .iter()
        .cloned()
        .fold(0.0_f32, |a, b| a.max(b.abs()));
    assert!(peak > 0.1, "peak={peak}, expected a non-silent signal");
}

#[test]
fn stretch_at_tempo_4x_roughly_quarters_duration() {
    let sample_rate = 48_000;
    let samples = sine(440.0, sample_rate, 2.0);
    let stretched = stretch_samples(&samples, sample_rate, 1, 4.0).unwrap();

    let ratio = samples.len() as f64 / stretched.len() as f64;
    assert!((ratio - 4.0).abs() < 0.1, "ratio={ratio}");
}

/// Regression: the `rubberband` filter can return the audio in planar
/// format even with packed input (see `aformat` after `rubberband` in
/// `stretch_samples`) — with a single channel (mono) the two formats
/// coincide in memory, so only a test with more channels really
/// exercises this path.
#[test]
fn stretch_stereo_roughly_halves_duration_and_stays_interleaved() {
    let sample_rate = 44_100;
    let channels = 2u16;
    let samples = sine(440.0, sample_rate, 5.0)
        .into_iter()
        .flat_map(|s| [s, s])
        .collect::<Vec<_>>();

    let stretched = stretch_samples(&samples, sample_rate, channels, 2.0).unwrap();

    assert_eq!(
        stretched.len() % channels as usize,
        0,
        "the interleaved buffer must stay an exact multiple of channels"
    );
    let ratio = samples.len() as f64 / stretched.len() as f64;
    assert!((ratio - 2.0).abs() < 0.05, "ratio={ratio}");

    let peak = stretched
        .iter()
        .cloned()
        .fold(0.0_f32, |a, b| a.max(b.abs()));
    assert!(peak > 0.1, "peak={peak}, expected a non-silent signal");
}

/// What the offline render feeds it: its chunks, not the whole input.
#[test]
fn a_stretch_fed_in_pieces_equals_one_fed_whole() {
    let samples: Vec<f32> = (0..96_000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    let whole = stretch_samples(&samples, 48_000, 2, 1.25).unwrap();
    let mut stretcher = Stretcher::new(48_000, 2, 1.25).unwrap();
    let mut pieces = Vec::new();
    for piece in samples.chunks(2 * 7_001) {
        stretcher.push(piece, &mut pieces).unwrap();
    }
    stretcher.finish(&mut pieces).unwrap();
    assert_eq!(pieces, whole);
}
