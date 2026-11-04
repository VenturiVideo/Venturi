use super::*;

#[test]
fn named_fields_and_legacy_ron_effects_default_safely() {
    let partial: PitchShift = ron::from_str("(semitones:12.0)").unwrap();
    assert_eq!(partial.ratio(), 2.0);
    assert!(partial.enabled);
    assert!(!partial.preserve_formants);
    let defaults = crate::EffectStack::default();
    let saved = ron::to_string(&defaults).unwrap();
    let field = format!(
        "pitch_shift:{},",
        ron::to_string(&defaults.pitch_shift).unwrap()
    );
    assert!(saved.contains(&field));
    let legacy = saved.replace(&field, "");
    let loaded: crate::EffectStack = ron::from_str(&legacy).unwrap();
    assert_eq!(loaded.pitch_shift, PitchShift::default());
}

#[test]
fn octaves_and_cents_have_the_expected_frequency_ratios() {
    let params = |semitones, cents| PitchShift {
        semitones,
        cents,
        ..Default::default()
    };
    assert_eq!(PitchShift::default().ratio(), 1.0);
    assert_eq!(params(12.0, 0.0).ratio(), 2.0);
    assert_eq!(params(-12.0, 0.0).ratio(), 0.5);
    assert_eq!(params(0.0, 100.0).ratio(), params(1.0, 0.0).ratio());
    assert_eq!(params(1.0, -100.0).ratio(), 1.0);
    assert!(
        !PitchShift {
            enabled: false,
            ..params(12.0, 0.0)
        }
        .is_active()
    );
}

#[test]
fn invalid_values_are_sanitized_before_reaching_the_audio_engine() {
    let params = PitchShift {
        semitones: f32::NAN,
        cents: f32::INFINITY,
        ..Default::default()
    };
    assert_eq!(params.sanitized(), PitchShift::default());
    let params = PitchShift {
        semitones: 200.0,
        cents: -500.0,
        ..Default::default()
    }
    .sanitized();
    assert_eq!((params.semitones, params.cents), (24.0, -100.0));
    assert!(params.ratio().is_finite());
}

#[test]
fn projects_round_trip_pitch_and_older_effects_load_without_shifting() {
    let mut effects = crate::EffectStack::default();
    effects.pitch_shift = PitchShift {
        enabled: false,
        semitones: -7.0,
        cents: 23.0,
        preserve_formants: true,
    };
    let saved = ron::to_string(&effects).unwrap();
    let loaded: crate::EffectStack = ron::from_str(&saved).unwrap();
    assert_eq!(loaded.pitch_shift, effects.pitch_shift);
    let mut older = serde_json::to_value(&effects).unwrap();
    older.as_object_mut().unwrap().remove("pitch_shift");
    let loaded: crate::EffectStack = serde_json::from_value(older).unwrap();
    assert_eq!(loaded.pitch_shift, PitchShift::default());
}
