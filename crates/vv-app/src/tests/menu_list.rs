use super::*;

#[test]
fn next_match_starts_at_the_top_without_focus() {
    assert_eq!(next_match(&['a', 'b', 'b'], None, 'b'), Some(1));
}

#[test]
fn next_match_cycles_and_wraps() {
    let firsts = ['b', 'a', 'b', 'c'];
    assert_eq!(next_match(&firsts, Some(0), 'b'), Some(2));
    assert_eq!(next_match(&firsts, Some(2), 'b'), Some(0));
}

#[test]
fn next_match_searches_after_a_non_matching_focus() {
    assert_eq!(next_match(&['b', 'a', 'b'], Some(1), 'b'), Some(2));
}

#[test]
fn next_match_without_matches_is_none() {
    assert_eq!(next_match(&['a', 'b'], Some(0), 'z'), None);
    assert_eq!(next_match(&[], None, 'a'), None);
}

#[test]
fn first_letter_skips_spaces_and_lowercases() {
    assert_eq!(first_letter("  Compressor"), Some('c'));
    assert_eq!(first_letter(""), None);
}
