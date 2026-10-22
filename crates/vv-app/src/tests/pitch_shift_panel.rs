use super::*;

#[test]
fn presets_are_distinct_translated_and_cover_both_directions() {
    let presets = builtin_presets();
    assert_eq!(presets[0].1, PitchShift::default());
    for (index, (key, params)) in presets.iter().enumerate() {
        assert_ne!(t!(*key), *key);
        assert_eq!(*params, params.sanitized());
        assert!(presets[..index].iter().all(|(_, other)| params != other));
    }
    assert!(presets.iter().any(|(_, params)| params.ratio() == 0.5));
    assert!(presets.iter().any(|(_, params)| params.ratio() == 2.0));
}

#[test]
fn individual_edits_preserve_the_other_pitch_controls() {
    let original = PitchShift {
        enabled: false,
        semitones: 12.0,
        cents: 50.0,
        preserve_formants: true,
    };
    for (edit, expected) in [
        (
            PitchShiftEdit::Enabled(true),
            PitchShift {
                enabled: true,
                ..original
            },
        ),
        (
            PitchShiftEdit::Semitones(-7.0),
            PitchShift {
                semitones: -7.0,
                ..original
            },
        ),
        (
            PitchShiftEdit::Cents(-25.0),
            PitchShift {
                cents: -25.0,
                ..original
            },
        ),
        (
            PitchShiftEdit::PreserveFormants(false),
            PitchShift {
                preserve_formants: false,
                ..original
            },
        ),
    ] {
        assert_eq!(edit.apply_to(original), expected);
    }
}

#[test]
fn presets_and_reset_replace_the_full_pitch_configuration() {
    let original = PitchShift {
        enabled: false,
        semitones: 3.0,
        cents: 23.0,
        preserve_formants: true,
    };
    for (_, preset) in builtin_presets() {
        assert_eq!(PitchShiftEdit::Replace(preset).apply_to(original), preset);
    }
    assert_eq!(
        PitchShiftEdit::Replace(PitchShift::default()).apply_to(original),
        PitchShift::default()
    );
}
