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
