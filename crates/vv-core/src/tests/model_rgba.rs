use super::*;

#[test]
fn hex_colors_round_trip() {
    for hex in ["#00FF00", "#00B140", "#000000", "#FFFFFF", "#123ABC"] {
        assert_eq!(Rgba::from_hex(hex).unwrap().to_hex(), hex);
    }
}

#[test]
fn hex_colors_are_read_with_or_without_the_hash_and_in_short_form() {
    let green = Rgba::from_hex("#00b140").unwrap();
    assert_eq!(Rgba::from_hex("00B140"), Some(green));
    assert_eq!(Rgba::from_hex("  #00B140 "), Some(green));
    assert_eq!(Rgba::from_hex("#0F0").unwrap().to_hex(), "#00FF00");
    assert_eq!(green.a, 1.0);
}

#[test]
fn malformed_hex_colors_are_rejected() {
    for text in [
        "", "#", "#00FF0", "#00FF000", "#GGHHII", "#00FF0é", "+00FF00",
    ] {
        assert_eq!(Rgba::from_hex(text), None, "{text:?}");
    }
}

#[test]
fn the_hex_of_a_color_rounds_and_clamps_its_channels() {
    let color = Rgba {
        r: 1.5,
        g: 0.5,
        b: -0.2,
        a: 0.3,
    };
    assert_eq!(color.to_hex(), "#FF8000");
}

#[test]
fn a_new_chroma_key_has_its_own_defaults() {
    let key = ClipFilter::new(FilterKind::ChromaKey);
    assert_eq!(key.color, DEFAULT_KEY_COLOR);
    assert_eq!(key.amount.default, DEFAULT_KEY_TOLERANCE);
    assert_eq!(key.radius.default, DEFAULT_KEY_SOFTNESS);
    assert_eq!(key.grow.default, DEFAULT_KEY_SPILL);
    assert_eq!(key.value_at(0), FilterValue::new(FilterKind::ChromaKey));
}
