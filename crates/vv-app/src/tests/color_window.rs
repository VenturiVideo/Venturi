use super::*;

#[test]
fn a_clip_is_isolated_on_its_own_track() {
    assert!(Isolation::shows(2, false, 2));
    assert!(!Isolation::shows(2, false, 1), "the track below goes");
    assert!(!Isolation::shows(2, false, 3), "the track above goes");
}

#[test]
fn an_adjustment_keeps_what_it_works_on() {
    assert!(Isolation::shows(2, true, 0));
    assert!(Isolation::shows(2, true, 2));
    assert!(!Isolation::shows(2, true, 3), "only the tracks above go");
}
