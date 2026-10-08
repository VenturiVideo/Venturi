use super::*;
use crate::GradePreset;
use crate::model::*;

#[test]
fn save_then_load_round_trips_a_project_with_clips_and_keyframes() {
    let mut project = Project::default();
    let timeline_id = project.timelines.insert(Timeline {
        name: "Timeline 1".into(),
        fps: Rational::new(30, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video), Track::new(TrackKind::Audio)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let media_id = project.media_pool.insert(MediaItem {
        path: "/tmp/example.mp4".into(),
        meta: MediaMeta {
            duration_frames: 100,
            fps: Rational::new(30, 1),
            width: 1920,
            height: 1080,
            has_video: true,
            has_audio: true,
            sample_rate: 48000,
            channels: 2,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 42,
        compound: None,
        folder: None,
    });

    let clip_id = project.alloc_clip_id();
    let mut effects = EffectStack::default();
    effects.gain_db.upsert(10, -6.0, Interpolation::Linear);
    let mut clip = Clip::from_source_range(
        clip_id,
        ClipSource::Media(media_id),
        0,
        50,
        0,
        Rational::one(),
    );
    clip.effects = effects;
    project.timelines[timeline_id].tracks[0].clips.push(clip);

    let dir = std::env::temp_dir().join("vv-core-persistence-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("project.vvproj");

    save_project(&project, &path).expect("save failed");
    let loaded = load_project(&path).expect("load failed");

    assert_eq!(loaded.timelines.len(), 1);
    let loaded_timeline = &loaded.timelines[timeline_id];
    assert_eq!(loaded_timeline.name, "Timeline 1");
    assert_eq!(loaded_timeline.fps, Rational::new(30, 1));
    assert_eq!(loaded_timeline.resolution, (1920, 1080));

    let loaded_clip = &loaded_timeline.tracks[0].clips[0];
    assert_eq!(loaded_clip.id, clip_id);
    assert_eq!(loaded_clip.source_out(), 50);
    assert_eq!(
        loaded_clip.effects.gain_db.keyframe_at(10),
        Some((-6.0, Interpolation::Linear))
    );

    let loaded_media = loaded.media_pool.get(media_id).unwrap();
    assert_eq!(
        loaded_media.path,
        std::path::PathBuf::from("/tmp/example.mp4")
    );
    assert_eq!(loaded_media.meta.sample_rate, 48000);
}

#[test]
fn load_project_from_malformed_ron_returns_an_error() {
    let dir = std::env::temp_dir().join("vv-core-persistence-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("broken.vvproj");
    std::fs::write(&path, "this is not valid RON {{{").unwrap();

    assert!(load_project(&path).is_err());
}

#[test]
fn load_project_from_missing_file_returns_an_error() {
    let dir = std::env::temp_dir().join("vv-core-persistence-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("does-not-exist.vvproj");
    let _ = std::fs::remove_file(&path);

    assert!(load_project(&path).is_err());
}

#[test]
fn title_params_survive_save_and_load() {
    let title = TitleParams {
        content: "Riga 1\nRiga 2".into(),
        anchor: (HAnchor::Left, VAnchor::Bottom),
        case: FontCase::Upper,
        ..Default::default()
    };
    let text = ron::to_string(&title).unwrap();
    let back: TitleParams = ron::from_str(&text).unwrap();
    assert_eq!(back, title);
}

#[test]
fn unknown_clip_colors_load_as_no_color() {
    #[derive(serde::Deserialize)]
    struct Holder {
        #[serde(deserialize_with = "crate::model::lenient_clip_color")]
        color: Option<ClipColor>,
    }
    for (text, expected) in [
        ("(color: Some(Chocolate))", None),
        ("(color: Some(Slate))", Some(ClipColor::Slate)),
        ("(color: None)", None),
    ] {
        let holder: Holder = ron::from_str(text).unwrap();
        assert_eq!(holder.color, expected, "{text}");
    }
}

#[test]
fn filters_saved_without_a_radius_load_with_the_default_one() {
    let filter: ClipFilter = ron::from_str("(kind: Exposure, enabled: true)").unwrap();
    assert_eq!(filter, ClipFilter::new(FilterKind::Exposure));
}

#[test]
fn the_old_grayscale_filter_loads_as_a_black_and_white_color_correction() {
    let filter: ClipFilter = ron::from_str("(kind: Grayscale, enabled: false)").unwrap();
    assert_eq!(filter.kind, FilterKind::ColorCorrection);
    assert!(!filter.enabled);
    assert_eq!(filter.grade.value_at(0), GradePreset::BlackAndWhite.value());
}

#[test]
fn every_filter_kind_survives_save_and_load() {
    use FilterKind::*;
    for kind in [ColorCorrection, BoxBlur, GaussianBlur, Exposure] {
        let filter = ClipFilter::new(kind);
        let text = ron::to_string(&filter).unwrap();
        assert_eq!(
            ron::from_str::<ClipFilter>(&text).unwrap(),
            filter,
            "{kind:?}"
        );
    }
}

#[test]
fn timelines_saved_without_markers_load_with_none() {
    let text = r#"(name: "T", fps: (num: 25, den: 1), resolution: (1920, 1080), tracks: [])"#;
    let timeline: Timeline = ron::from_str(text).unwrap();
    assert!(timeline.markers.is_empty());

    let text = r#"(name: "T", fps: (num: 25, den: 1), resolution: (1920, 1080), tracks: [],
        markers: [(id: (3), start: 12)])"#;
    let timeline: Timeline = ron::from_str(text).unwrap();
    assert_eq!(timeline.markers[0].color, Marker::default_color());
    assert_eq!(timeline.markers[0].duration, 0);
}

#[test]
fn media_folders_survive_save_and_load() {
    let mut project = Project::default();
    let outer = project.folders.insert(MediaFolder {
        name: "Footage".into(),
        parent: None,
    });
    let inner = project.folders.insert(MediaFolder {
        name: "Day 1".into(),
        parent: Some(outer),
    });
    let media = project.media_pool.insert(MediaItem {
        path: "/tmp/a.mp4".into(),
        meta: MediaMeta {
            duration_frames: 10,
            fps: Rational::new(25, 1),
            width: 64,
            height: 48,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: Some(inner),
    });
    let path = std::env::temp_dir().join("vv-core-folders-test.vvproj");
    save_project(&project, &path).unwrap();
    let loaded = load_project(&path).unwrap();

    assert_eq!(loaded.folders[inner].name, "Day 1");
    assert_eq!(loaded.folders[inner].parent, Some(outer));
    assert_eq!(loaded.media_pool[media].folder, Some(inner));
}

#[test]
fn projects_saved_without_a_precision_load_with_high() {
    let mut project = Project::default();
    project.precision = ProcessingPrecision::Standard;
    let text = ron::to_string(&project).unwrap();
    let old = text.replace(",precision:Standard", "");
    assert_ne!(old, text);
    let project: Project = ron::from_str(&old).unwrap();
    assert_eq!(project.precision, ProcessingPrecision::High);
}
