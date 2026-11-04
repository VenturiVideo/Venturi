use super::*;

fn sine(freq: f32, sample_rate: u32, secs: f32) -> Vec<f32> {
    let n = (sample_rate as f32 * secs) as usize;
    (0..n)
        .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate as f32).sin())
        .collect()
}

#[test]
fn stretch_at_tempo_2x_roughly_halves_duration() {
    if skip_without_rubberband() {
        return;
    }
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
    if skip_without_rubberband() {
        return;
    }
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
    if skip_without_rubberband() {
        return;
    }
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
    if skip_without_rubberband() {
        return;
    }
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
