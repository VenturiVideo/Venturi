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
