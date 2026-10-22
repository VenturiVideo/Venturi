use super::*;
use crate::test_support::TempDir;

fn temp_dir(name: &str) -> TempDir {
    TempDir::new(&format!("vv-forced-relink-{name}"))
}

fn meta(duration_frames: i64, width: u32, height: u32) -> MediaMeta {
    MediaMeta {
        duration_frames,
        fps: vv_core::Rational::new(25, 1),
        width,
        height,
        has_video: true,
        has_audio: false,
        sample_rate: 0,
        channels: 0,
        audio_streams: 0,
        file: Default::default(),
    }
}

fn reference(path: &str, meta: MediaMeta) -> Reference {
    Reference {
        media_id: <MediaId as vv_core::Id>::from_raw(0),
        path: path.into(),
        meta,
        waveform: None,
    }
}

fn only(criteria: &[Criterion]) -> Criteria {
    Criteria {
        enabled: criteria.iter().copied().collect(),
        ..Criteria::default()
    }
}

fn clip(path: &Path, size: &str, frames: u32) {
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size={size}:rate=25"),
            "-frames:v",
            &frames.to_string(),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        path,
    );
}

#[test]
fn name_alone_matches_across_a_different_extension() {
    let dir = temp_dir("name");
    let found = dir.join("rana.mp4");
    std::fs::write(&found, b"not even a video").unwrap();
    let r = reference("/home/user/Videos/rana.mov", meta(10, 64, 48));

    assert!(matches(
        &only(&[Criterion::Name]),
        &r,
        &mut Candidate::new(found.clone())
    ));
    assert!(
        !matches(
            &only(&[Criterion::Name, Criterion::Extension]),
            &r,
            &mut Candidate::new(found)
        ),
        "every enabled criterion must hold"
    );
}

#[test]
fn size_matches_within_the_tolerance_only() {
    let dir = temp_dir("size");
    let found = dir.join("rospo.mov");
    std::fs::write(&found, vec![0u8; 10_000]).unwrap();
    let mut m = meta(10, 64, 48);
    m.file.size_bytes = Some(10_000 + 2048);
    let r = reference("/old/rana.mov", m);

    let mut criteria = only(&[Criterion::Size]);
    criteria.size_tolerance_kib = 1;
    assert!(!matches(&criteria, &r, &mut Candidate::new(found.clone())));
    criteria.size_tolerance_kib = 2;
    assert!(matches(&criteria, &r, &mut Candidate::new(found)));
}

#[test]
fn a_criterion_the_project_did_not_record_never_matches() {
    let dir = temp_dir("unknown");
    let found = dir.join("rana.mov");
    std::fs::write(&found, b"x").unwrap();
    let r = reference("/old/rana.mov", meta(10, 64, 48));

    assert!(!Criterion::Size.known_for(&r));
    assert!(!matches(
        &only(&[Criterion::Name, Criterion::Size]),
        &r,
        &mut Candidate::new(found)
    ));
}

#[test]
fn no_criteria_matches_nothing() {
    let r = reference("/old/rana.mov", meta(10, 64, 48));
    assert!(!only(&[]).is_searchable());
    assert!(!matches(
        &only(&[]),
        &r,
        &mut Candidate::new("/old/rana.mov".into())
    ));
}

#[test]
fn waveform_similarity_ignores_resolution_and_rejects_other_shapes() {
    let shape: Vec<f32> = (0..1000).map(|i| ((i as f32) / 40.0).sin().abs()).collect();
    let coarser: Vec<f32> = shape.iter().step_by(3).copied().collect();
    let other: Vec<f32> = (0..1000).map(|i| ((i as f32) / 7.0).cos().abs()).collect();

    assert!(waveform_similarity(&shape, &shape) > 0.99);
    assert!(waveform_similarity(&shape, &coarser) > 0.9);
    assert!(waveform_similarity(&shape, &other) < 0.5);
    assert_eq!(waveform_similarity(&shape, &[0.5; 100]), 0.0, "flat");
    assert_eq!(waveform_similarity(&shape, &[]), 0.0);
}

/// Example 2 of the task: another name, another resolution, same length
/// within the tolerances.
#[test]
fn search_finds_a_renamed_file_by_length_and_frame_count() {
    let dir = temp_dir("search");
    std::fs::create_dir_all(dir.join("backup")).unwrap();
    let renamed = dir.join("backup").join("rospo.mp4");
    clip(&renamed, "64x36", 26);
    clip(&dir.join("other.mp4"), "64x36", 40);
    std::fs::write(dir.join("notes.txt"), b"not media").unwrap();
    let references = vec![
        reference("/home/user/Videos/rana.mov", meta(25, 1920, 1080)),
        reference("/home/user/Videos/ghost.mov", meta(500, 1920, 1080)),
    ];
    let mut criteria = only(&[Criterion::Duration, Criterion::Frames]);
    criteria.duration_tolerance_ms = 50;
    criteria.frames_tolerance = 1;

    let found = search(
        &references,
        dir.path(),
        &criteria,
        &SearchProgress::default(),
    )
    .unwrap();

    assert_eq!(found[0].len(), 1, "{:?}", found[0]);
    assert_eq!(found[0][0].path, renamed);
    assert_eq!(
        found[0][0].meta.as_ref().map(|m| m.duration_frames),
        Some(26),
        "the probed meta travels with the match"
    );
    assert!(found[1].is_empty());
}

#[test]
fn search_puts_the_same_name_first_among_several_matches() {
    let dir = temp_dir("order");
    std::fs::write(dir.join("aaa.mp4"), b"x").unwrap();
    std::fs::write(dir.join("rana.mp4"), b"x").unwrap();
    let references = vec![reference("/old/rana.mov", meta(10, 64, 48))];

    let found = search(
        &references,
        dir.path(),
        &only(&[Criterion::Extension]),
        &SearchProgress::default(),
    )
    .unwrap();
    assert!(found[0].is_empty(), "mov vs mp4");

    let references = vec![reference("/old/rana.mp4", meta(10, 64, 48))];
    let found = search(
        &references,
        dir.path(),
        &only(&[Criterion::Extension]),
        &SearchProgress::default(),
    )
    .unwrap();
    let names: Vec<_> = found[0]
        .iter()
        .map(|m| m.path.file_name().unwrap())
        .collect();
    assert_eq!(names, ["rana.mp4", "aaa.mp4"]);
}

#[test]
fn tags_are_recorded_by_the_probe_and_matched_case_insensitively() {
    let dir = temp_dir("tags");
    let path = dir.join("song.m4a");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=duration=0.5",
            "-metadata",
            "title=Rana Song",
            "-metadata",
            "artist=The Frogs",
        ],
        &path,
    );
    let probed = vv_media::probe_media(&path).unwrap();
    assert_eq!(probed.file.title.as_deref(), Some("Rana Song"));
    assert_eq!(probed.file.artist.as_deref(), Some("The Frogs"));
    assert_eq!(probed.file.audio_codec.as_deref(), Some("aac"));
    assert_eq!(
        probed.file.size_bytes,
        Some(std::fs::metadata(&path).unwrap().len())
    );

    let mut old = probed.clone();
    old.file.title = Some("rana song ".into());
    let r = reference("/old/track.flac", old);
    assert!(matches(
        &only(&[Criterion::Title, Criterion::Artist, Criterion::Codec]),
        &r,
        &mut Candidate::new(path.clone())
    ));

    let mut criteria = only(&[Criterion::Tag]);
    criteria.tag_key = "ARTIST".into();
    criteria.tag_value = "the frogs".into();
    assert!(matches(&criteria, &r, &mut Candidate::new(path.clone())));
    criteria.tag_value = "the toads".into();
    assert!(!matches(&criteria, &r, &mut Candidate::new(path)));
}

#[test]
fn a_cancelled_search_returns_nothing() {
    let dir = temp_dir("cancel");
    std::fs::write(dir.join("rana.mp4"), b"x").unwrap();
    let progress = SearchProgress::default();
    progress.cancel.store(true, Ordering::Relaxed);
    let references = vec![reference("/old/rana.mov", meta(10, 64, 48))];
    assert!(
        search(
            &references,
            dir.path(),
            &only(&[Criterion::Name]),
            &progress
        )
        .is_none()
    );
}
