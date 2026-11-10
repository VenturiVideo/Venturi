use super::*;
use crate::model::*;

fn project() -> (Project, TimelineId, MediaId) {
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path: "/tmp/a video.mp4".into(),
        meta: MediaMeta {
            duration_frames: 1000,
            fps: Rational::new(30_000, 1001),
            width: 1920,
            height: 1080,
            has_video: true,
            has_audio: true,
            sample_rate: 48_000,
            channels: 2,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let timeline = project.timelines.insert(Timeline {
        name: "Timeline 1".into(),
        fps: Rational::new(30, 1),
        resolution: (1920, 1080),
        tracks: vec![
            Track::new(TrackKind::Video),
            Track::new(TrackKind::Video),
            Track::new(TrackKind::Audio),
        ],
        markers: Vec::new(),
        master: Default::default(),
    });
    (project, timeline, media)
}

#[test]
fn exports_tracks_in_compositing_order_with_gaps_between_clips() {
    let (mut project, timeline_id, media) = project();
    let rate = Rational::conform_rate(Rational::new(30, 1), Rational::new(30_000, 1001));
    let tl = &mut project.timelines[timeline_id];
    tl.tracks[0].clips.push(Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(media),
        100,
        400,
        30,
        rate,
    ));
    let mut color =
        Clip::from_source_range(ClipId(2), ClipSource::SolidColor, 0, 60, 0, Rational::one());
    color.effects.color = Some(Keyframed::constant(Rgba {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    }));
    tl.tracks[1].clips.push(color);
    tl.tracks[2].muted = true;

    let otio = timeline_to_otio(&project, timeline_id, None);
    let tracks = otio["tracks"]["children"].as_array().unwrap();
    let names: Vec<&str> = tracks.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["V1", "V2", "A1"]);
    assert_eq!(tracks[2]["kind"], "Audio");
    assert_eq!(tracks[2]["enabled"], false, "muted track");

    let v1 = tracks[0]["children"].as_array().unwrap();
    assert_eq!(v1[0]["OTIO_SCHEMA"], "Gap.1");
    assert_eq!(v1[0]["source_range"]["duration"]["value"], 30.0);
    let clip = &v1[1];
    assert_eq!(clip["name"], "a video.mp4");
    assert_eq!(clip["source_range"]["start_time"]["value"], 100.0);
    assert_eq!(clip["source_range"]["start_time"]["rate"], 30.0);
    assert_eq!(clip["source_range"]["duration"]["value"], 300.0);
    let reference = &clip["media_references"]["DEFAULT_MEDIA"];
    assert_eq!(reference["target_url"], "file:///tmp/a%20video.mp4");
    assert_eq!(reference["available_range"]["duration"]["value"], 1000.0);

    let generator = &tracks[1]["children"][0]["media_references"]["DEFAULT_MEDIA"];
    assert_eq!(generator["generator_kind"], "Solid Color");
    let block = &generator["parameters"]["Resolve_OTIO"][0];
    assert_eq!(block["Effect Name"], "Solid Color");
    assert_eq!(block["Parameters"][1]["Parameter Value"], "#ff0000");
}

/// After a split mid source frame the right half starts where the left
/// one ends, both on the timeline and in the media.
#[test]
fn a_split_conformed_clip_exports_contiguous_source_ranges() {
    let (mut project, timeline_id, media) = project();
    let rate = Rational::conform_rate(Rational::new(30, 1), Rational::new(30_000, 1001));
    project.timelines[timeline_id].tracks[0]
        .clips
        .push(Clip::from_source_range(
            ClipId(1),
            ClipSource::Media(media),
            0,
            1000,
            0,
            rate,
        ));
    let mut split = crate::SplitClip::new(timeline_id, 0, ClipId(1), 500);
    crate::Command::apply(&mut split, &mut project);

    let otio = timeline_to_otio(&project, timeline_id, None);
    let v1 = otio["tracks"]["children"][0]["children"]
        .as_array()
        .unwrap();
    assert_eq!(v1.len(), 2, "no gap between the two halves");
    let start = |i: usize| {
        v1[i]["source_range"]["start_time"]["value"]
            .as_f64()
            .unwrap()
    };
    let len = |i: usize| v1[i]["source_range"]["duration"]["value"].as_f64().unwrap();
    assert_eq!(start(0) + len(0), start(1));
    assert_eq!(len(0) + len(1), 1001.0);
}

/// Parameter IDs, units and shape copied from a file exported by
/// Resolve: this is the only thing its importer reads back.
#[test]
fn exports_the_transform_as_resolve_effects() {
    let (mut project, timeline_id, media) = project();
    let mut clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(media),
        0,
        100,
        0,
        Rational::one(),
    );
    let t = &mut clip.effects.transform;
    t.track_mut(TransformParam::ZoomX).default = 1.07;
    t.track_mut(TransformParam::PositionX).default = 96.0;
    t.track_mut(TransformParam::PositionY).default = -54.0;
    t.track_mut(TransformParam::Rotation).default = 7.8;
    t.track_mut(TransformParam::AnchorX).default = 192.0;
    t.track_mut(TransformParam::CropTop).default = 108.0;
    t.track_mut(TransformParam::Opacity).default = 80.0;
    t.flip = [true, false];
    clip.set_speed(Rational::from_percent(170.57), Rational::one());
    clip.effects.blend_mode = BlendMode::Screen;
    clip.fade_in = 12;
    project.timelines[timeline_id].tracks[0].clips.push(clip);

    let otio = timeline_to_otio(&project, timeline_id, None);
    let effects = otio["tracks"]["children"][0]["children"][0]["effects"]
        .as_array()
        .unwrap();
    let named = |name: &str| {
        effects
            .iter()
            .find(|e| e["metadata"]["Resolve_OTIO"]["Effect Name"] == name)
            .unwrap_or_else(|| panic!("no effect {name}"))
            .clone()
    };
    let raw = |effect: &Value, id: &str| {
        effect["metadata"]["Resolve_OTIO"]["Parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["Parameter ID"] == id)
            .unwrap_or_else(|| panic!("no parameter {id}"))["Parameter Value"]
            .clone()
    };
    // The values are f32 widened to f64: comparing them exactly would
    // only measure the widening.
    let value = |effect: &Value, id: &str| (raw(effect, id).as_f64().unwrap() * 1e6).round() / 1e6;

    let transform = named("Transform");
    assert_eq!(transform["effect_name"], "Resolve Effect");
    assert_eq!(value(&transform, "transformationZoomX"), 1.07);
    assert_eq!(
        value(&transform, "transformationPan"),
        0.05,
        "96 px out of 1920"
    );
    assert_eq!(
        value(&transform, "transformationTilt"),
        -0.05,
        "-54 px out of 1080"
    );
    assert_eq!(
        value(&transform, "transformationRotationAngle"),
        -7.8,
        "opposite direction"
    );
    let anchor = raw(&transform, "transformationAnchorPoint");
    assert!((anchor[0].as_f64().unwrap() - 0.1).abs() < 1e-6);
    assert_eq!(anchor[1].as_f64().unwrap(), 0.0);
    assert!(
        transform["metadata"]["Resolve_OTIO"]["Parameters"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["Parameter ID"] != "transformationZoomY"),
        "parameters at default are omitted, as Resolve does"
    );
    assert_eq!(raw(&transform, "transformationFlipX"), true);
    assert!(
        transform["metadata"]["Resolve_OTIO"]["Parameters"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["Parameter ID"] != "transformationFlipY"),
        "an inactive flip is omitted"
    );
    assert_eq!(value(&named("Cropping"), "cropTop"), 0.1);
    assert_eq!(value(&named("Composite"), "opacity"), 80.0);
    assert_eq!(raw(&named("Composite"), "composite mode"), 5, "Screen");
    assert_eq!(value(&named("Video Faders"), "videoFaderIn"), 12.0);

    // The speed is the one thing Resolve reads from the standard schema.
    let warp = &effects[0];
    assert_eq!(warp["OTIO_SCHEMA"], "LinearTimeWarp.1");
    assert_eq!(
        (warp["time_scalar"].as_f64().unwrap() * 1e6).round() / 1e6,
        1.7057
    );
}

#[test]
fn exports_the_keyframes_on_the_timeline_frames_of_the_clip() {
    let (mut project, timeline_id, media) = project();
    let mut clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(media),
        30,
        130,
        0,
        Rational::one(),
    );
    clip.effects
        .transform
        .track_mut(TransformParam::ZoomX)
        .upsert(40, 2.0, Interpolation::Linear);
    project.timelines[timeline_id].tracks[0].clips.push(clip);

    let otio = timeline_to_otio(&project, timeline_id, None);
    let keys = &otio["tracks"]["children"][0]["children"][0]["effects"][0]["metadata"]["Resolve_OTIO"]
        ["Parameters"][0]["Key Frames"];
    assert_eq!(
        keys["10"]["Value"], 2.0,
        "source frame 40, tenth of the clip"
    );
}

/// "Around the text" is a shorthand we have and Resolve does not: the
/// axes left at 0 travel as the fraction of the frame the box really
/// covers.
#[test]
fn a_background_around_the_text_becomes_an_explicit_size() {
    let (mut project, timeline_id, _) = project();
    let mut clip = Clip::from_source_range(ClipId(1), ClipSource::Text, 0, 60, 0, Rational::one());
    let mut title = TitleParams::default();
    title.background.enabled = true;
    title.background.width = 0.0;
    title.background.height = 0.0;
    clip.effects.title = Some(title);
    project.timelines[timeline_id].tracks[0].clips.push(clip);

    let measure = |_: &TitleParams| crate::TitleMetrics {
        block: (440.0, 176.0),
        padding: 20.0,
    };
    let otio = timeline_to_otio(&project, timeline_id, Some(&measure));
    let blocks = &otio["tracks"]["children"][0]["children"][0]["media_references"]["DEFAULT_MEDIA"]
        ["parameters"]["Resolve_OTIO"];
    let background = blocks
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["Effect Name"] == "Background")
        .unwrap();
    let value = |id: &str| {
        background["Parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["Parameter ID"] == id)
            .unwrap()["Parameter Value"]
            .as_f64()
            .unwrap()
    };
    // 440 px of text plus 10% margin, plus 20 px of padding per side.
    assert!(
        (value("backgroundWidth") - 524.0 / 1920.0).abs() < 1e-6,
        "524 px out of 1920"
    );
    assert!(
        (value("backgroundHeight") - 0.2).abs() < 1e-6,
        "216 px out of 1080"
    );
    // 0.1 of the rectangle's short side (216 px) is 21.6 px, which for
    // Resolve is a fraction of the frame height.
    assert!(
        (value("backgroundCornerRadius") - 0.02).abs() < 1e-6,
        "21.6 px out of 1080"
    );

    // Without a measurement there is nothing better than the frame.
    let otio = timeline_to_otio(&project, timeline_id, None);
    let background = otio["tracks"]["children"][0]["children"][0]["media_references"]
        ["DEFAULT_MEDIA"]["parameters"]["Resolve_OTIO"][3]["Parameters"][4]["Parameter Value"]
        .as_f64()
        .unwrap();
    assert_eq!(background, 1.0);
}

/// Resolve reads the parameters of a generator only from a file that
/// declares itself one of its own: without these two fields the colour
/// and the title arrive at their defaults.
#[test]
fn marks_the_file_as_written_by_resolve() {
    let (project, timeline_id, _) = project();
    let otio = timeline_to_otio(&project, timeline_id, None);
    assert_eq!(
        otio["metadata"]["Resolve_OTIO"]["Resolve OTIO Meta Version"],
        "1.0"
    );
    for track in otio["tracks"]["children"].as_array().unwrap() {
        assert_eq!(track["metadata"]["Resolve_OTIO"]["Locked"], false);
    }
}

/// A vertical clip in a horizontal timeline: Resolve measures the pan
/// on the clip as it is fitted into the frame, so a shift of 700 px on
/// a 1080x2400 source that lands 486 px wide is far more than 700/1920.
#[test]
fn normalizes_the_position_on_the_clip_not_on_the_frame() {
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path: "/tmp/vertical.mp4".into(),
        meta: MediaMeta {
            duration_frames: 1000,
            fps: Rational::new(30, 1),
            width: 1080,
            height: 2400,
            has_video: true,
            has_audio: false,
            sample_rate: 48_000,
            channels: 2,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 2,
        compound: None,
        folder: None,
    });
    let timeline_id = project.timelines.insert(Timeline {
        name: "Vertical".into(),
        fps: Rational::new(30, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let mut clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(media),
        0,
        100,
        0,
        Rational::one(),
    );
    clip.effects
        .transform
        .track_mut(TransformParam::PositionX)
        .default = -700.0;
    project.timelines[timeline_id].tracks[0].clips.push(clip);

    let otio = timeline_to_otio(&project, timeline_id, None);
    let pan = otio["tracks"]["children"][0]["children"][0]["effects"][0]["metadata"]
        ["Resolve_OTIO"]["Parameters"][0]["Parameter Value"]
        .as_f64()
        .unwrap();
    assert_eq!(
        (pan * 1e4).round() / 1e4,
        -1.4403,
        "-700 out of 486 px of clip"
    );
    assert_eq!(
        (pan * 1920.0).round(),
        -2765.0,
        "the value Resolve's inspector shows in pixels"
    );
}

#[test]
fn exports_a_transition_as_an_item_that_takes_no_time() {
    let (mut project, timeline_id, media) = project();
    let mut clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(media),
        0,
        100,
        0,
        Rational::one(),
    );
    clip.effects.transition_in = Some(Transition {
        kind: TransitionKind::Push,
        duration: 24,
        direction: PushDirection::Left,
        ease: Ease::InOut,
        curve: 0.5,
    });
    project.timelines[timeline_id].tracks[0].clips.push(clip);

    let otio = timeline_to_otio(&project, timeline_id, None);
    let children = otio["tracks"]["children"][0]["children"]
        .as_array()
        .unwrap();
    assert_eq!(children.len(), 2);
    let transition = &children[0];
    assert_eq!(transition["OTIO_SCHEMA"], "Transition.1");
    assert_eq!(transition["in_offset"]["value"], 0.0);
    assert_eq!(
        transition["out_offset"]["value"], 24.0,
        "reaches into the following clip"
    );
    assert_eq!(
        transition["metadata"]["Resolve_OTIO"]["Transition Type"],
        "Push"
    );
    assert_eq!(
        transition["metadata"]["venturi"]["transition"]["direction"],
        "Left"
    );

    // Without the progress curve Resolve holds the transition at 0.
    let curve = &transition["metadata"]["Resolve_OTIO"]["Effects"]["Parameters"][1];
    assert_eq!(curve["Parameter ID"], "transitionCustomCurvesKeyframes");
    let keys = &curve["Key Frames"];
    assert_eq!(keys["0"]["Value"], 0.0);
    assert_eq!(keys["24"]["Value"], 1.0);
    let handle =
        |key: &Value, name: &str| (key[name][name][0].as_f64().unwrap() * 1e3).round() / 1e3;
    assert_eq!(handle(&keys["0"], "OutBez"), 7.2, "0.5 * 0.6 * 24");
    assert_eq!(handle(&keys["24"], "InBez"), -7.2);
}

#[test]
fn keeps_what_otio_cannot_represent_in_metadata() {
    let (mut project, timeline_id, media) = project();
    let mut clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(media),
        0,
        10,
        0,
        Rational::one(),
    );
    clip.audio_stream_index = 1;
    clip.linked_group = Some(LinkGroupId(7));
    clip.effects.gain_db = Keyframed::constant(-6.0);
    clip.display_color = Some(crate::model::ClipColor::Indigo);
    project.timelines[timeline_id].tracks[2].clips.push(clip);

    let otio = timeline_to_otio(&project, timeline_id, None);
    let meta = &otio["tracks"]["children"][2]["children"][0]["metadata"]["venturi"];
    assert_eq!(meta["audio_stream_index"], 1);
    assert_eq!(meta["linked_group"], 7);
    assert_eq!(meta["effects"]["gain_db"]["default"], -6.0);
    assert_eq!(meta["display_color"], "Indigo");
}

#[test]
fn camera_shake_round_trips_through_venturi_otio_metadata() {
    let (mut project, timeline, _) = project();
    let mut clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::SolidColor,
        0,
        100,
        0,
        Rational::one(),
    );
    let mut filter = ClipFilter::new(FilterKind::CameraShake);
    filter.shake.seed = 123;
    filter
        .shake
        .track_mut(crate::ShakeParam::MotionScale)
        .upsert(12, 0.25, Interpolation::EaseInOut);
    clip.effects.filters.push(filter.clone());
    project.timelines[timeline].tracks[0].clips.push(clip);
    let value = timeline_to_otio(&project, timeline, None);
    let loaded = crate::otio::project_from_otio(
        &value,
        std::path::Path::new("/tmp"),
        &mut |_| panic!("no external media"),
        None,
    )
    .unwrap();
    let timeline = loaded.project.timelines.values().next().unwrap();
    assert_eq!(timeline.tracks[0].clips[0].effects.filters, vec![filter]);
}
