use super::*;

#[test]
#[cfg(target_os = "linux")]
fn case_insensitive_matches_every_spelling() {
    assert_eq!(
        case_insensitive(&["mp4", "tiff"]),
        ["[mM][pP]4", "[tT][iI][fF][fF]"]
    );
}

#[test]
#[cfg(not(target_os = "linux"))]
fn case_insensitive_keeps_plain_extensions() {
    assert_eq!(case_insensitive(&["mp4", "tiff"]), ["mp4", "tiff"]);
}
