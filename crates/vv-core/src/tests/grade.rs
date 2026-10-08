use super::*;

#[test]
fn neutral_values_follow_the_params() {
    for param in GradeParam::ALL {
        assert_eq!(GradeValue::NEUTRAL.get(param), param.neutral(), "{param:?}");
        assert_eq!(GradeParam::ALL[param.index()], param);
    }
}

#[test]
fn black_and_white_only_drops_the_global_saturation() {
    let value = GradePreset::BlackAndWhite.value();
    for param in GradeParam::ALL {
        let expected = if param == GradeParam::Saturation {
            0.0
        } else {
            param.neutral()
        };
        assert_eq!(value.get(param), expected, "{param:?}");
    }
}

#[test]
fn tracks_survive_save_and_load() {
    let mut tracks = GradeTracks::default();
    tracks
        .track_mut(GradeParam::MidtonesX)
        .upsert(10, 0.5, crate::Interpolation::Linear);
    let text = ron::to_string(&tracks).unwrap();
    assert_eq!(ron::from_str::<GradeTracks>(&text).unwrap(), tracks);
}

#[test]
fn files_with_fewer_params_load_the_missing_ones_neutral() {
    let tracks: GradeTracks =
        ron::from_str("(params: [(keyframes: [], default: 0.25), (keyframes: [], default: -0.5)])")
            .unwrap();
    let value = tracks.value_at(0);
    assert_eq!(value.get(GradeParam::ShadowsX), 0.25);
    assert_eq!(value.get(GradeParam::ShadowsY), -0.5);
    assert_eq!(
        value.get(GradeParam::HighRange),
        GradeParam::HighRange.neutral()
    );
}

#[test]
fn a_keyframed_param_interpolates() {
    let mut tracks = GradeTracks::default();
    let track = tracks.track_mut(GradeParam::OffsetLuma);
    track.upsert(0, 0.0, crate::Interpolation::Linear);
    track.upsert(10, 1.0, crate::Interpolation::Linear);
    assert_eq!(tracks.value_at(5).get(GradeParam::OffsetLuma), 0.5);
    assert_eq!(tracks.value_at(5).get(GradeParam::Saturation), 1.0);
}

#[test]
fn a_chroma_shift_leaves_the_luma_unchanged() {
    for (x, y) in [(1.0, 0.0), (0.0, 1.0), (-0.6, 0.8)] {
        let shift = chroma_shift(x, y);
        let luma: f32 = shift.iter().zip(LUMA_WEIGHTS).map(|(c, w)| c * w).sum();
        assert!(luma.abs() < 1e-4, "({x}, {y}): {luma}");
        assert!(shift.iter().any(|c| c.abs() > 0.1));
    }
}

#[test]
fn the_chroma_shift_grows_with_the_square_of_the_distance() {
    let edge = chroma_shift(0.0, 1.0)[0];
    let half = chroma_shift(0.0, 0.5)[0];
    assert!((half - edge / 4.0).abs() < 1e-6, "{half} vs {edge}");
}

#[test]
fn wheel_for_chroma_inverts_wheel_chroma() {
    for (x, y) in [(0.3, -0.2), (-0.7, 0.1), (0.0, 0.9)] {
        let (bx, by) = wheel_for_chroma(wheel_chroma(x, y));
        assert!(
            (bx - x).abs() < 1e-5 && (by - y).abs() < 1e-5,
            "({x}, {y}) -> ({bx}, {by})"
        );
    }
    let (x, y) = wheel_for_chroma([10.0, 0.0]);
    assert_eq!((x, y), (1.0, 0.0), "out of reach: at the edge");
}

#[test]
fn the_range_weights_sum_to_one() {
    for luma in [0.0, 0.2, 1.0 / 3.0, 0.5, 0.7, 1.0] {
        let weights = range_weights(luma, 1.0 / 3.0, 2.0 / 3.0);
        assert!(
            (weights.iter().sum::<f32>() - 1.0).abs() < 1e-6,
            "{luma}: {weights:?}"
        );
    }
    assert!(range_weights(0.05, 1.0 / 3.0, 2.0 / 3.0)[0] > 0.99);
    assert!(range_weights(0.95, 1.0 / 3.0, 2.0 / 3.0)[2] > 0.99);
}

/// A blue cast in the midtones only: the midtones wheel moves against it,
/// the shadows and highlights ones stay.
#[test]
fn auto_balance_cancels_a_cast_in_its_range() {
    let cast = [0.47, 0.5, 0.6];
    let pixels = (0..100)
        .map(|_| cast)
        .chain((0..100).map(|_| [0.05, 0.05, 0.05]));
    let balanced = auto_balance(&GradeValue::NEUTRAL, pixels);
    let luma: f32 = cast.iter().zip(LUMA_WEIGHTS).map(|(c, w)| c * w).sum();
    let wanted = [-(cast[2] - luma) / 1.8556, -(cast[0] - luma) / 1.5748];
    let [cb, cr] = wheel_chroma(
        balanced.get(GradeParam::MidtonesX),
        balanced.get(GradeParam::MidtonesY),
    );
    let weight = range_weights(luma, 1.0 / 3.0, 2.0 / 3.0)[1];
    assert!(weight > 0.99);
    assert!(
        (cb - wanted[0]).abs() < 1e-3 && (cr - wanted[1]).abs() < 1e-3,
        "{cb} {cr} vs {wanted:?}"
    );
    assert_eq!(balanced.get(GradeParam::ShadowsX), 0.0, "neutral shadows");
    assert_eq!(balanced.get(GradeParam::HighlightsX), 0.0, "no highlights");
}

#[test]
fn auto_balance_ignores_clipped_pixels_and_leaves_the_rest_alone() {
    let pixels = (0..100)
        .map(|_| [1.0, 1.0, 0.6])
        .chain((0..100).map(|_| [0.0, 0.0, 0.2]));
    let mut grade = GradeValue::NEUTRAL;
    grade.set(GradeParam::OffsetX, 0.3);
    grade.set(GradeParam::MidtonesLuma, 0.2);
    assert_eq!(auto_balance(&grade, pixels), grade);
}
