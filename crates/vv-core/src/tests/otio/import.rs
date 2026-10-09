use super::*;
use crate::model::{BlendMode, ClipId, CrossTransition};
use crate::otio::timeline_to_otio;
use serde_json::json;

fn meta(fps: Rational, duration_frames: FrameIdx) -> MediaMeta {
    MediaMeta {
        duration_frames,
        fps,
        width: 1280,
        height: 720,
        has_video: true,
        has_audio: true,
        sample_rate: 48_000,
        channels: 2,
        audio_streams: 1,
        file: Default::default(),
    }
}

fn probe_from(media: Vec<(&'static str, MediaMeta)>) -> impl FnMut(&Path) -> ProbeResult {
    move |path| {
        media
            .iter()
            .find(|(p, _)| Path::new(p) == path)
            .map(|(_, m)| (m.clone(), 42))
            .ok_or_else(|| "file not found".to_owned())
    }
}

fn span(clip: &Clip) -> (FrameIdx, FrameIdx, FrameIdx, Rational) {
    (
        clip.timeline_start,
        clip.source_offset,
        clip.timeline_len,
        clip.rate(),
    )
}

/// Exporting and importing a Venturi project returns the same clips,
/// even split mid source frame.
#[test]
fn a_venturi_export_imports_back_unchanged() {
    let media_meta = meta(Rational::new(30_000, 1001), 1000);
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path: "/tmp/a.mp4".into(),
        meta: media_meta.clone(),
        content_hash: 42,
        compound: None,
        folder: None,
    });
    let timeline_id = project.timelines.insert(Timeline {
        name: "Montaggio".into(),
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
    let rate = Rational::conform_rate(Rational::new(30, 1), media_meta.fps);
    let group = project.alloc_link_group_id();
    let mut video =
        Clip::from_source_range(ClipId(1), ClipSource::Media(media), 100, 900, 20, rate);
    video.linked_group = Some(group);
    let mut audio = video.clone();
    audio.id = ClipId(2);
    audio.audio_stream_index = 1;
    audio
        .effects
        .gain_db
        .upsert(10, -6.0, Interpolation::Linear);
    let mut color = Clip::from_source_range(
        ClipId(3),
        ClipSource::SolidColor,
        0,
        45,
        300,
        Rational::one(),
    );
    color.effects.color = Some(Keyframed::constant(Rgba {
        r: 0.2,
        g: 0.4,
        b: 0.6,
        a: 1.0,
    }));
    color.fade_in = 5;
    color.fade_out = 7;
    color.effects.transition_in = Some(Transition {
        kind: TransitionKind::Push,
        duration: 9,
        direction: PushDirection::Up,
        ease: Ease::In,
        curve: 0.25,
    });
    let tl = &mut project.timelines[timeline_id];
    tl.tracks[0].clips.push(video);
    tl.tracks[1].clips.push(color);
    tl.tracks[2].clips.push(audio);
    tl.tracks[2].muted = true;
    let mut split = crate::SplitClip::new(timeline_id, 0, ClipId(1), 500);
    crate::Command::apply(&mut split, &mut project);
    let track = &mut project.timelines[timeline_id].tracks[0];
    let (left_clip, right_clip) = (track.clips[0].id, track.clips[1].id);
    track.crossings.push(CrossTransition {
        left_clip,
        right_clip,
        transition: Transition {
            kind: TransitionKind::Push,
            duration: 10,
            direction: PushDirection::Right,
            ease: Ease::None,
            curve: 0.5,
        },
    });

    let otio = timeline_to_otio(&project, timeline_id, None);
    let mut probe = probe_from(vec![("/tmp/a.mp4", media_meta.clone())]);
    let imported = project_from_otio(&otio, Path::new("/"), &mut probe, None).unwrap();
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);

    let original = &project.timelines[timeline_id];
    let (_, back) = imported.project.timelines.iter().next().unwrap();
    assert_eq!(back.name, "Montaggio");
    assert_eq!(back.fps, original.fps);
    assert_eq!(back.resolution, original.resolution);
    assert_eq!(back.tracks.len(), 3);
    for (a, b) in original.tracks.iter().zip(&back.tracks) {
        assert_eq!((a.kind, a.muted), (b.kind, b.muted));
        let spans_a: Vec<_> = a.clips.iter().map(span).collect();
        let spans_b: Vec<_> = b.clips.iter().map(span).collect();
        assert_eq!(spans_a, spans_b);
    }
    let audio = &back.tracks[2].clips[0];
    assert_eq!(audio.audio_stream_index, 1);
    assert_eq!(audio.effects.gain_db.value_at(10), -6.0);
    assert!(audio.linked_group.is_some());
    assert_eq!(back.tracks[0].clips[0].linked_group, audio.linked_group);
    assert_eq!(
        back.tracks[0].clips[1].linked_group, None,
        "right half unlinked"
    );
    let generated = &back.tracks[1].clips[0];
    let color = generated.effects.color.as_ref().unwrap().default;
    assert_eq!((color.r, color.g, color.b), (0.2, 0.4, 0.6));
    assert_eq!((generated.fade_in, generated.fade_out), (5, 7));
    assert_eq!(
        generated
            .effects
            .transition_in
            .as_ref()
            .map(|t| (t.direction, t.duration, t.ease)),
        Some((PushDirection::Up, 9, Ease::In)),
        "the transition comes back whole from metadata.venturi"
    );
    let cut = &back.tracks[0];
    assert_eq!(cut.crossings.len(), 1);
    let crossing = &cut.crossings[0];
    assert_eq!(
        (crossing.left_clip, crossing.right_clip),
        (cut.clips[0].id, cut.clips[1].id)
    );
    assert_eq!(
        (crossing.transition.duration, crossing.transition.direction),
        (10, PushDirection::Right)
    );
    assert!(cut.clips[0].effects.transition_out.is_none());
    assert!(cut.clips[1].effects.transition_in.is_none());
}

/// Generators go back and forth through Resolve's own blocks: the same
/// file, with our metadata stripped, has to rebuild the title from the
/// Qt rich text and the colour from the hex.
#[test]
fn reads_back_the_generators_without_our_metadata() {
    let measure = |_: &crate::model::TitleParams| crate::TitleMetrics {
        block: (440.0, 176.0),
        padding: 20.0,
    };
    let mut project = Project::default();
    let timeline_id = project.timelines.insert(Timeline {
        name: "Generatori".into(),
        fps: Rational::new(30, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video), Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let mut color =
        Clip::from_source_range(ClipId(1), ClipSource::SolidColor, 0, 60, 0, Rational::one());
    color.effects.color = Some(Keyframed::constant(Rgba {
        r: 0.231_372_55,
        g: 0.709_803_94,
        b: 0.207_843_14,
        a: 1.0,
    }));
    let mut text = Clip::from_source_range(ClipId(2), ClipSource::Text, 0, 60, 0, Rational::one());
    let title = crate::model::TitleParams {
        content: "Two\nlines & <>".into(),
        font_family: "Open Sans".into(),
        font_weight: 700,
        italic: true,
        underline: true,
        size: 72.0,
        align: crate::model::TextAlign::Left,
        anchor: (crate::model::HAnchor::Right, crate::model::VAnchor::Bottom),
        position: [192.0, -108.0],
        background: crate::model::TitleBackground {
            enabled: true,
            width: 0.25,
            height: 0.2,
            corner_radius: 0.1,
            ..Default::default()
        },
        ..Default::default()
    };
    text.effects.title = Some(title.clone());
    project.timelines[timeline_id].tracks[0].clips.push(color);
    project.timelines[timeline_id].tracks[1].clips.push(text);

    let mut otio = timeline_to_otio(&project, timeline_id, Some(&measure));
    for track in otio["tracks"]["children"].as_array_mut().unwrap() {
        for clip in track["children"].as_array_mut().unwrap() {
            clip["metadata"]["venturi"] = json!(null);
        }
    }
    let mut probe = probe_from(vec![]);
    let imported = project_from_otio(&otio, Path::new("/"), &mut probe, Some(&measure)).unwrap();
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);

    let (_, tl) = imported.project.timelines.iter().next().unwrap();
    let color = tl.tracks[0].clips[0]
        .effects
        .color
        .as_ref()
        .unwrap()
        .default;
    assert_eq!(
        ((color.r * 255.0).round(), (color.g * 255.0).round()),
        (59.0, 181.0)
    );

    let back = tl.tracks[1].clips[0].effects.title.as_ref().unwrap();
    assert_eq!(
        back.content, title.content,
        "text and lines from the HTML blob"
    );
    assert_eq!(back.font_family, "Open Sans");
    assert_eq!((back.font_weight, back.size), (700, 72.0));
    assert!(back.italic && back.underline && !back.strikethrough);
    assert_eq!(back.align, crate::model::TextAlign::Left);
    assert_eq!(back.anchor, title.anchor, "index 8 of the 3x3 grid");
    assert!((back.position[0] - 192.0).abs() < 0.01 && (back.position[1] + 108.0).abs() < 0.01);
    assert!(
        (back.background.corner_radius - title.background.corner_radius).abs() < 1e-4,
        "the radius comes back in our units: {}",
        back.background.corner_radius
    );
}

/// Resolve marks an adjustment clip only with a parameterless effect of
/// Type 74 on a `MissingReference`: the name can be anything.
#[test]
fn a_resolve_adjustment_clip_is_recognised_by_its_type_74_effect() {
    let missing = json!({ "OTIO_SCHEMA": "MissingReference.1", "metadata": {} });
    let marker = json!({
        "OTIO_SCHEMA": "Effect.1",
        "effect_name": "Resolve Effect",
        "metadata": { "Resolve_OTIO": {
            "Effect Name": "Effect",
            "Enabled": true,
            "Parameters": [],
            "Type": 74,
        }},
    });
    let clip = |name: &str, effects: Value| {
        json!({
            "OTIO_SCHEMA": "Clip.2",
            "name": name,
            "source_range": range(0.0, 30.0, 30.0),
            "effects": effects,
            "media_references": { "DEFAULT_MEDIA": missing.clone() },
            "active_media_reference_key": "DEFAULT_MEDIA",
        })
    };
    let otio = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": "Adjust",
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "children": [{
                "OTIO_SCHEMA": "Track.1",
                "kind": "Video",
                "children": [
                    clip("Renamed", json!([marker])),
                    clip("Fusion Composition", json!([])),
                ],
            }],
        },
    });
    let mut probe = probe_from(vec![]);
    let imported = project_from_otio(&otio, Path::new("/"), &mut probe, None).unwrap();
    let (_, tl) = imported.project.timelines.iter().next().unwrap();
    let clips = &tl.tracks[0].clips;
    assert_eq!(clips.len(), 2);
    assert!(clips[0].is_adjustment());
    assert!(!clips[0].disabled);
    // Without the effect it is a composition: a disabled placeholder.
    assert!(matches!(clips[1].source, ClipSource::Text));
    assert!(clips[1].disabled);
    assert!(matches!(
        imported.warnings.as_slice(),
        [OtioWarning::Placeholder { clip }] if clip == "Fusion Composition"
    ));
}

/// What we export must read as an adjustment clip without our metadata too,
/// i.e. as Resolve would see it, with its transform.
#[test]
fn an_exported_adjustment_clip_reads_back_without_our_metadata() {
    let mut project = Project::default();
    let timeline_id = project.timelines.insert(Timeline {
        name: "Adjust".into(),
        fps: Rational::new(30, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let mut clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Adjustment,
        0,
        60,
        10,
        Rational::one(),
    );
    clip.effects.transform = crate::model::TransformTracks::constant(crate::model::Transform {
        zoom: [1.5, 1.5],
        ..Default::default()
    });
    project.timelines[timeline_id].tracks[0].clips.push(clip);

    let mut otio = timeline_to_otio(&project, timeline_id, None);
    for track in otio["tracks"]["children"].as_array_mut().unwrap() {
        for clip in track["children"].as_array_mut().unwrap() {
            clip["metadata"]["venturi"] = json!(null);
        }
    }
    let mut probe = probe_from(vec![]);
    let imported = project_from_otio(&otio, Path::new("/"), &mut probe, None).unwrap();
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    let (_, tl) = imported.project.timelines.iter().next().unwrap();
    let back = &tl.tracks[0].clips[0];
    assert!(back.is_adjustment());
    assert!(!back.disabled, "a MissingReference, but not a placeholder");
    assert_eq!((back.timeline_start, back.timeline_len), (10, 60));
    assert_eq!(back.effects.transform.value_at(0).zoom, [1.5, 1.5]);
}

/// A clip exported by Resolve: the transform lives in `Effect.1` items
/// with normalized values and keyframes on the frames of the clip.
#[test]
fn reads_the_transform_of_a_resolve_clip() {
    let parameter = |id: &str, value: Value| {
        json!({
            "Parameter ID": id,
            "Parameter Value": value,
            "Default Parameter Value": 0.0,
            "Variant Type": "Double",
            "Key Frames": {},
        })
    };
    let effect = |name: &str, parameters: Value| {
        json!({
            "OTIO_SCHEMA": "Effect.1",
            "name": "",
            "effect_name": "Resolve Effect",
            "metadata": { "Resolve_OTIO": {
                "Effect Name": name,
                "Name": name,
                "Enabled": true,
                "Parameters": parameters,
            }},
        })
    };
    let otio = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": "From Resolve",
        "tracks": { "OTIO_SCHEMA": "Stack.1", "children": [{
            "OTIO_SCHEMA": "Track.1",
            "kind": "Video",
            "children": [{
                "OTIO_SCHEMA": "Clip.2",
                "name": "one",
                "source_range": range(0.0, 100.0, 24.0),
                "media_references": { "DEFAULT_MEDIA": {
                    "OTIO_SCHEMA": "ExternalReference.1",
                    "target_url": B_ROLL,
                    "available_range": range(0.0, 2400.0, 24.0),
                }},
                "effects": [
                    json!({
                        "OTIO_SCHEMA": "LinearTimeWarp.1",
                        "name": "",
                        "effect_name": "",
                        "time_scalar": 2.0,
                    }),
                    effect("Transform", json!([
                        parameter("transformationZoomX", json!(1.07)),
                        parameter("transformationPan", json!(0.05)),
                        parameter("transformationTilt", json!(-0.05)),
                        parameter("transformationRotationAngle", json!(7.8)),
                        json!({
                            "Parameter ID": "transformationAnchorPoint",
                            "Parameter Value": [0.1, 0.0],
                            "Default Parameter Value": [0.0, 0.0],
                            "Variant Type": "POINTF",
                            "Key Frames": {
                                "0": { "Value": [0.1, 0.0], "Variant Type": "POINTF" },
                                "10": { "Value": [0.5, 0.0], "Variant Type": "POINTF" },
                            },
                        }),
                        json!({
                            "Parameter ID": "transformationFlipY",
                            "Parameter Value": true,
                            "Default Parameter Value": false,
                            "Variant Type": "Bool",
                        }),
                    ])),
                    effect("Cropping", json!([parameter("cropTop", json!(0.25))])),
                    effect("Composite", json!([
                        parameter("opacity", json!(80.0)),
                        json!({
                            "Parameter ID": "composite mode",
                            "Parameter Value": 5,
                            "Default Parameter Value": 0,
                            "Variant Type": "UInt",
                        }),
                    ])),
                    effect("Video Faders", json!([parameter("videoFaderIn", json!(12.0))])),
                ],
            }],
        }]},
    });
    let mut probe = probe_from(vec![(
        "/media/b roll.mov",
        meta(Rational::new(24, 1), 2400),
    )]);
    let imported = project_from_otio(&otio, Path::new("/media"), &mut probe, None).unwrap();
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);

    let (_, tl) = imported.project.timelines.iter().next().unwrap();
    let clip = &tl.tracks[0].clips[0];
    assert_eq!(clip.speed(), Rational::new(2, 1));
    assert_eq!(
        (clip.timeline_len, clip.source_frame_at(99)),
        (100, 198),
        "the OTIO duration is timeline time"
    );
    assert_eq!(clip.effects.transform.flip, [false, true]);
    let t = clip.effects.transform.value_at(0);
    assert_eq!(t.zoom[0], 1.07);
    assert_eq!(t.position, [64.0, -36.0], "denormalized on 1280x720");
    assert_eq!(t.rotation, -7.8, "opposite direction to Resolve's");
    assert_eq!(t.anchor[0], 128.0);
    assert_eq!(t.crop[1], 180.0, "crop is in media pixels");
    assert_eq!(t.opacity, 80.0);
    assert_eq!(clip.effects.blend_mode, BlendMode::Screen);
    assert_eq!(clip.fade_in, 12);
    assert_eq!(
        clip.effects.transform.value_at(20).anchor[0],
        640.0,
        "keyframe at timeline frame 10 is source frame 20 at 2x"
    );
}

fn rt(value: f64, rate: f64) -> Value {
    json!({ "OTIO_SCHEMA": "RationalTime.1", "value": value, "rate": rate })
}

fn range(start: f64, duration: f64, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "start_time": rt(start, rate),
        "duration": rt(duration, rate),
    })
}

const B_ROLL: &str = "file:///media/b%20roll.mov";

/// Source at 24 fps from frame `start` for `duration`.
fn clip_1(name: &str, url: &str, start: f64, duration: f64, enabled: bool) -> Value {
    json!({
        "OTIO_SCHEMA": "Clip.1",
        "name": name,
        "source_range": range(start, duration, 24.0),
        "enabled": enabled,
        "effects": [],
        "media_reference": {
            "OTIO_SCHEMA": "ExternalReference.1",
            "target_url": url,
            // Media with a start timecode of 01:00:00:00 at 24 fps.
            "available_range": range(86_400.0, 2400.0, 24.0),
        },
    })
}

/// File from another editor: `Clip.1`, times in the media rate with a
/// start timecode, transitions, disabled clips and missing media (kept
/// offline); video and audio of the same stretch must be relinked.
#[test]
fn a_foreign_file_imports_with_warnings_for_what_is_skipped() {
    let fps = 24_000.0 / 1001.0;
    let otio = json!({
        "OTIO_SCHEMA": "SerializableCollection.1",
        "children": [{
            "OTIO_SCHEMA": "Timeline.1",
            "name": "From Resolve",
            "global_start_time": rt(86_400.0, fps),
            "tracks": {
                "OTIO_SCHEMA": "Stack.1",
                "children": [
                    {
                        "OTIO_SCHEMA": "Track.1",
                        "kind": "Video",
                        "children": [
                            { "OTIO_SCHEMA": "Gap.1", "source_range": range(0.0, 24.0, fps) },
                            clip_1("one", B_ROLL, 86_448.0, 48.0, true),
                            { "OTIO_SCHEMA": "Transition.1", "in_offset": rt(6.0, fps) },
                            clip_1("disabled", B_ROLL, 86_400.0, 24.0, false),
                            clip_1("lost", "file:///media/missing.mov", 86_400.0, 24.0, true),
                            clip_1("two", "b roll.mov", 86_400.0, 12.0, true),
                        ],
                    },
                    {
                        "OTIO_SCHEMA": "Track.1",
                        "kind": "Audio",
                        "enabled": false,
                        "children": [
                            { "OTIO_SCHEMA": "Gap.1", "source_range": range(0.0, 24.0, fps) },
                            clip_1("one", B_ROLL, 86_448.0, 48.0, true),
                        ],
                    },
                ],
            },
        }],
    });
    let mut probe = probe_from(vec![(
        "/media/b roll.mov",
        meta(Rational::new(24, 1), 2400),
    )]);
    let imported = project_from_otio(&otio, Path::new("/media"), &mut probe, None).unwrap();

    assert_eq!(imported.warnings.len(), 1, "{:?}", imported.warnings);
    assert!(
        matches!(&imported.warnings[0], OtioWarning::MediaUnreadable { path, .. } if path.ends_with("missing.mov"))
    );
    assert_eq!(
        imported.project.media_pool.len(),
        2,
        "same file probed once, plus the offline one"
    );
    let offline = imported
        .project
        .media_pool
        .values()
        .find(|m| m.path.ends_with("missing.mov"))
        .unwrap();
    assert_eq!(
        (
            offline.meta.duration_frames,
            offline.meta.fps,
            offline.meta.width
        ),
        (2400, Rational::new(24, 1), 0),
        "described by available_range, resolution unknown"
    );
    assert!(offline.meta.has_video && !offline.meta.has_audio);

    let (_, tl) = imported.project.timelines.iter().next().unwrap();
    assert_eq!(tl.fps, Rational::new(24_000, 1001));
    assert_eq!(tl.resolution, (1280, 720), "from the first video media");
    let video = &tl.tracks[0].clips;
    assert_eq!(video.len(), 4);
    assert_eq!((video[0].timeline_start, video[0].timeline_len), (24, 48));
    assert_eq!(video[0].source_in(), 48, "from the media start timecode");
    assert!(!video[0].disabled);
    assert!(video[1].disabled);
    assert_eq!(video[1].timeline_start, 24 + 48);
    assert!(!video[2].disabled, "offline, not disabled");
    assert_eq!(video[3].timeline_start, 24 + 48 + 24 + 24);
    assert_eq!(video[3].source_in(), 0, "relative path");
    let transition = video[0]
        .effects
        .transition_out
        .as_ref()
        .expect("outgoing transition");
    assert_eq!(
        transition.duration, 6,
        "in_offset reaches into the previous clip"
    );

    let audio = &tl.tracks[1];
    assert!(audio.muted);
    assert!(video[0].linked_group.is_some());
    assert_eq!(audio.clips[0].linked_group, video[0].linked_group);
    assert_eq!(video[3].linked_group, None);
}

/// Resolve's Text+ and Fusion compositions come as an empty
/// `MissingReference`: a disabled title holds their place. An adjustment
/// clip without its effect is left out.
#[test]
fn missing_references_become_disabled_placeholder_titles() {
    let missing = |name: &str| {
        json!({
            "OTIO_SCHEMA": "Clip.2",
            "name": name,
            "source_range": range(0.0, 24.0, 24.0),
            "media_references": { "DEFAULT_MEDIA": { "OTIO_SCHEMA": "MissingReference.1" } },
            "active_media_reference_key": "DEFAULT_MEDIA",
        })
    };
    let otio = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "tracks": { "OTIO_SCHEMA": "Stack.1", "children": [{
            "OTIO_SCHEMA": "Track.1",
            "kind": "Video",
            "children": [missing("Text+"), missing("Adjustment Clip")],
        }]},
    });
    let mut probe = probe_from(vec![]);
    let imported = project_from_otio(&otio, Path::new("/"), &mut probe, None).unwrap();

    assert_eq!(
        imported.warnings,
        vec![
            OtioWarning::Placeholder {
                clip: "Text+".into()
            },
            OtioWarning::UnsupportedReference {
                clip: "Adjustment Clip".into(),
                schema: "MissingReference".into()
            },
        ]
    );
    let (_, tl) = imported.project.timelines.iter().next().unwrap();
    let clips = &tl.tracks[0].clips;
    assert_eq!(clips.len(), 1);
    assert!(matches!(clips[0].source, ClipSource::Text));
    assert!(clips[0].disabled);
    assert_eq!(clips[0].effects.title.as_ref().unwrap().content, "Text+");
    assert_eq!(clips[0].timeline_len, 24);
}

fn resolve_effect(name: &str, enabled: bool, parameters: Value) -> Value {
    json!({
        "OTIO_SCHEMA": "Effect.1",
        "effect_name": "Resolve Effect",
        "metadata": { "Resolve_OTIO": {
            "Effect Name": name,
            "Enabled": enabled,
            "Parameters": parameters,
        }},
    })
}

fn volume(value: f64, keyframes: Value) -> Value {
    json!([{
        "Parameter ID": "volume",
        "Default Parameter Value": 0.0,
        "Parameter Value": value,
        "Key Frames": keyframes,
    }])
}

/// Resolve exports the whole effect stack of every clip: volume
/// becomes gain, disabled and default ones disappear, the rest is
/// summarized in one warning per effect. Groups and audio streams come
/// from its metadata.
#[test]
fn resolve_effects_links_and_channels_are_translated() {
    let resolve_clip = |start: f64, effects: Value, link: u64, source_track: u64| {
        let mut clip = clip_1("c", B_ROLL, 86_400.0 + start, 24.0, true);
        clip["effects"] = effects;
        clip["metadata"] = json!({ "Resolve_OTIO": {
            "Link Group ID": link,
            "Channels": [
                { "Source Channel ID": 0, "Source Track ID": source_track },
                { "Source Channel ID": 1, "Source Track ID": source_track },
            ],
        }});
        clip
    };
    let zoom = json!([{
        "Parameter ID": "zoom",
        "Default Parameter Value": 1.0,
        "Parameter Value": 1.5,
    }]);
    let track = |kind: &str, children: Vec<Value>| json!({ "OTIO_SCHEMA": "Track.1", "kind": kind, "children": children });
    let otio = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "global_start_time": rt(0.0, 24.0),
        "tracks": { "OTIO_SCHEMA": "Stack.1", "children": [
            track("Video", vec![
                resolve_clip(0.0, json!([
                    resolve_effect("Transform", true, json!([])),
                    resolve_effect("Dynamic Zoom", false, zoom.clone()),
                    resolve_effect("Zoom", true, zoom.clone()),
                ]), 5, 0),
                resolve_clip(24.0, json!([resolve_effect("Zoom", true, zoom)]), 6, 0),
            ]),
            track("Audio", vec![
                resolve_clip(0.0, json!([
                    resolve_effect(
                        "Fairlight Clip Volume and Fades",
                        true,
                        volume(3.5, json!({})),
                    ),
                ]), 5, 1),
                resolve_clip(24.0, json!([resolve_effect(
                    "Fairlight Clip Volume and Fades",
                    true,
                    volume(0.0, json!({ "0": { "Value": -6.0 }, "12": { "Value": 0.0 } })),
                )]), 6, 1),
            ]),
        ]},
    });
    let mut probe = probe_from(vec![(
        "/media/b roll.mov",
        meta(Rational::new(24, 1), 2400),
    )]);
    let imported = project_from_otio(&otio, Path::new("/"), &mut probe, None).unwrap();

    assert_eq!(
        imported.warnings,
        [OtioWarning::EffectIgnored {
            effect: "Zoom".into(),
            clips: 2
        }]
    );
    let (_, tl) = imported.project.timelines.iter().next().unwrap();
    let (video, audio) = (&tl.tracks[0].clips, &tl.tracks[1].clips);
    assert_eq!(audio[0].effects.gain_db.value_at(0), 3.5);
    let gain = &audio[1].effects.gain_db;
    assert_eq!(
        gain.value_at(24),
        -6.0,
        "keyframe on the first source frame of the clip"
    );
    assert_eq!(gain.value_at(36), 0.0);
    assert_eq!(gain.value_at(30), -3.0);
    assert_eq!(audio[0].audio_stream_index, 1);
    assert_eq!(video[0].linked_group, audio[0].linked_group);
    assert_eq!(video[1].linked_group, audio[1].linked_group);
    assert_ne!(video[0].linked_group, video[1].linked_group);
}

fn audio_only_meta() -> MediaMeta {
    MediaMeta {
        duration_frames: 300,
        fps: Rational::new(30, 1),
        width: 0,
        height: 0,
        has_video: false,
        has_audio: true,
        sample_rate: 48_000,
        channels: 2,
        audio_streams: 1,
        file: Default::default(),
    }
}

/// An audio-only media comes back identical from an export of ours, reads
/// at any rate from another editor, and on a video track is discarded
/// with a warning.
#[test]
fn audio_only_media_round_trips_and_is_refused_on_video_tracks() {
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path: "/tmp/voice.wav".into(),
        meta: audio_only_meta(),
        content_hash: 42,
        compound: None,
        folder: None,
    });
    let fps = Rational::new(25, 1);
    let timeline_id = project.timelines.insert(Timeline {
        name: "Voice".into(),
        fps,
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video), Track::new(TrackKind::Audio)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let rate = Rational::conform_rate(fps, Rational::new(30, 1));
    project.timelines[timeline_id].tracks[1]
        .clips
        .push(Clip::from_source_range(
            ClipId(1),
            ClipSource::Media(media),
            30,
            270,
            10,
            rate,
        ));
    let mut split = crate::SplitClip::new(timeline_id, 1, ClipId(1), 77);
    crate::Command::apply(&mut split, &mut project);

    let otio = timeline_to_otio(&project, timeline_id, None);
    let mut probe = probe_from(vec![("/tmp/voice.wav", audio_only_meta())]);
    let imported = project_from_otio(&otio, Path::new("/"), &mut probe, None).unwrap();
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    let (_, back) = imported.project.timelines.iter().next().unwrap();
    let spans = |t: &Timeline| t.tracks[1].clips.iter().map(span).collect::<Vec<_>>();
    assert_eq!(spans(back), spans(&project.timelines[timeline_id]));

    let wav_clip = |start_samples: f64| {
        json!({
            "OTIO_SCHEMA": "Clip.2",
            "name": "voice",
            "source_range": range(start_samples, 48_000.0, 48_000.0),
            "media_references": { "DEFAULT_MEDIA": {
                "OTIO_SCHEMA": "ExternalReference.1",
                "target_url": "file:///tmp/voice.wav",
            }},
            "active_media_reference_key": "DEFAULT_MEDIA",
        })
    };
    let track = |kind: &str| json!({ "OTIO_SCHEMA": "Track.1", "kind": kind, "children": [wav_clip(24_000.0)] });
    let foreign = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "global_start_time": rt(0.0, 25.0),
        "tracks": { "OTIO_SCHEMA": "Stack.1", "children": [track("Video"), track("Audio")] },
    });
    let imported = project_from_otio(&foreign, Path::new("/"), &mut probe, None).unwrap();
    assert_eq!(imported.warnings.len(), 1, "{:?}", imported.warnings);
    assert!(matches!(
        imported.warnings[0],
        OtioWarning::AudioOnlyOnVideoTrack { .. }
    ));
    let (_, tl) = imported.project.timelines.iter().next().unwrap();
    assert!(tl.tracks[0].clips.is_empty());
    let clip = &tl.tracks[1].clips[0];
    assert_eq!(
        (clip.timeline_len, clip.source_offset),
        (25, 13),
        "1 s from 0.5 s, at 25 fps"
    );
    assert_eq!(clip.rate(), rate);
    assert_eq!(tl.resolution, (1920, 1080));
}

#[test]
fn a_file_without_timelines_is_an_error() {
    let mut probe = probe_from(vec![]);
    let not_a_timeline = json!({ "OTIO_SCHEMA": "Clip.2" });
    let result = project_from_otio(&not_a_timeline, Path::new("/"), &mut probe, None);
    assert!(matches!(result, Err(OtioError::Format(_))));
}

#[test]
fn fps_from_float_recognises_ntsc_rates() {
    assert_eq!(Rational::from_fps(25.0), Rational::new(25, 1));
    assert_eq!(Rational::from_fps(29.97), Rational::new(30_000, 1001));
    assert_eq!(
        Rational::from_fps(30_000.0 / 1001.0),
        Rational::new(30_000, 1001)
    );
    assert_eq!(Rational::from_fps(23.976), Rational::new(24_000, 1001));
    assert_eq!(Rational::from_fps(59.94), Rational::new(60_000, 1001));
    assert_eq!(Rational::from_fps(12.5), Rational::new(25, 2));
}

#[test]
fn media_url_count_counts_each_file_once() {
    let reference = |url: &str| json!({ "OTIO_SCHEMA": "ExternalReference.1", "target_url": url });
    let otio = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "tracks": { "OTIO_SCHEMA": "Stack.1", "children": [{
            "OTIO_SCHEMA": "Track.1",
            "children": [
                { "OTIO_SCHEMA": "Clip.1", "media_reference": reference("a.mp4") },
                { "OTIO_SCHEMA": "Clip.1", "media_reference": reference("a.mp4") },
                { "OTIO_SCHEMA": "Clip.2", "media_references": { "DEFAULT_MEDIA": reference("b.mov") } },
            ],
        }]},
    });
    assert_eq!(media_url_count(&otio), 2);
}

/// With our metadata the speed comes back exact; without, as Resolve reads
/// it, from `time_scalar` and a `source_range` starting in media time.
#[test]
fn a_clip_speed_round_trips_with_and_without_our_metadata() {
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path: "/media/b roll.mov".into(),
        meta: meta(Rational::new(24, 1), 2400),
        content_hash: 42,
        compound: None,
        folder: None,
    });
    let timeline_id = project.timelines.insert(Timeline {
        name: "Speed".into(),
        fps: Rational::new(24, 1),
        resolution: (1280, 720),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let mut clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(media),
        0,
        1,
        12,
        Rational::one(),
    );
    clip.set_speed(Rational::from_percent(250.0), Rational::one());
    clip.pitch_correction = true;
    clip.source_offset = clip.rate().scale_round(100);
    clip.timeline_len = 40;
    project.timelines[timeline_id].tracks[0].clips.push(clip);
    let original = project.timelines[timeline_id].tracks[0].clips[0].clone();

    let import = |otio: &Value| {
        let mut probe = probe_from(vec![(
            "/media/b roll.mov",
            meta(Rational::new(24, 1), 2400),
        )]);
        let imported = project_from_otio(otio, Path::new("/"), &mut probe, None).unwrap();
        assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
        let (_, tl) = imported.project.timelines.iter().next().unwrap();
        tl.tracks[0].clips[0].clone()
    };
    let mut otio = timeline_to_otio(&project, timeline_id, None);
    let ours = import(&otio);
    assert_eq!(span(&ours), span(&original));
    assert_eq!(
        (ours.speed(), ours.pitch_correction),
        (Rational::new(5, 2), true)
    );

    for track in otio["tracks"]["children"].as_array_mut().unwrap() {
        for clip in track["children"].as_array_mut().unwrap() {
            clip["metadata"]["venturi"] = json!(null);
        }
    }
    let foreign = import(&otio);
    assert_eq!(foreign.speed(), Rational::new(5, 2));
    assert_eq!(foreign.source_in(), original.source_in(), "media time");
    assert_eq!(span(&foreign), span(&original));
}

#[test]
fn an_unknown_clip_color_imports_as_no_color() {
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path: "/media/a.mov".into(),
        meta: meta(Rational::new(24, 1), 240),
        content_hash: 42,
        compound: None,
        folder: None,
    });
    let timeline_id = project.timelines.insert(Timeline {
        name: "Colors".into(),
        fps: Rational::new(24, 1),
        resolution: (1280, 720),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let mut clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(media),
        0,
        48,
        0,
        Rational::one(),
    );
    clip.display_color = Some(crate::model::ClipColor::Slate);
    project.timelines[timeline_id].tracks[0].clips.push(clip);

    let import = |otio: &Value| {
        let mut probe = probe_from(vec![("/media/a.mov", meta(Rational::new(24, 1), 240))]);
        let imported = project_from_otio(otio, Path::new("/"), &mut probe, None).unwrap();
        let (_, tl) = imported.project.timelines.iter().next().unwrap();
        tl.tracks[0].clips[0].display_color
    };
    let mut otio = timeline_to_otio(&project, timeline_id, None);
    assert_eq!(import(&otio), Some(crate::model::ClipColor::Slate));
    otio["tracks"]["children"][0]["children"][0]["metadata"]["venturi"]["display_color"] =
        json!("Chocolate");
    assert_eq!(import(&otio), None);
}

/// Resolve writes a transition between two clips as one item reaching into
/// both: it is a single crossing, not one transition per edge.
#[test]
fn a_transition_between_two_clips_imports_as_one_crossing() {
    let push = |into_previous: f64, into_next: f64| {
        json!({
            "OTIO_SCHEMA": "Transition.1",
            "in_offset": rt(into_previous, 24.0),
            "out_offset": rt(into_next, 24.0),
        })
    };
    let otio = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": "Push",
        "global_start_time": rt(0.0, 24.0),
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "children": [{
                "OTIO_SCHEMA": "Track.1",
                "kind": "Video",
                "children": [
                    clip_1("a", B_ROLL, 86_400.0, 48.0, true),
                    push(6.0, 6.0),
                    clip_1("b", B_ROLL, 86_448.0, 48.0, true),
                    push(4.0, 4.0),
                    { "OTIO_SCHEMA": "Gap.1", "source_range": range(0.0, 24.0, 24.0) },
                ],
            }],
        },
    });
    let mut probe = probe_from(vec![(
        "/media/b roll.mov",
        meta(Rational::new(24, 1), 2400),
    )]);
    let imported = project_from_otio(&otio, Path::new("/media"), &mut probe, None).unwrap();
    let (_, tl) = imported.project.timelines.iter().next().unwrap();
    let track = &tl.tracks[0];
    let (a, b) = (&track.clips[0], &track.clips[1]);
    assert!(a.effects.transition_out.is_none());
    assert!(b.effects.transition_in.is_none());
    assert_eq!(track.crossings.len(), 1);
    let crossing = &track.crossings[0];
    assert_eq!((crossing.left_clip, crossing.right_clip), (a.id, b.id));
    assert_eq!(crossing.transition.duration, 12);
    assert_eq!(
        b.effects.transition_out.as_ref().map(|t| t.duration),
        Some(4),
        "followed by a gap: only the half inside the clip"
    );
}

#[test]
fn masks_of_an_adjustment_clip_round_trip_in_our_metadata() {
    let mut project = Project::default();
    let timeline_id = project.timelines.insert(Timeline {
        name: "Masks".into(),
        fps: Rational::new(24, 1),
        resolution: (1280, 720),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let mut clip =
        Clip::from_source_range(ClipId(1), ClipSource::Adjustment, 0, 48, 0, Rational::one());
    let mut ellipse = crate::ClipMask::new(crate::MaskShape::Ellipse, (1280, 720));
    ellipse.invert = true;
    ellipse
        .track_mut(crate::MaskParam::Feather)
        .upsert(10, 40.0, Interpolation::Linear);
    clip.effects.masks = vec![
        ellipse,
        crate::ClipMask::new(crate::MaskShape::Path, (1280, 720)),
    ];
    project.timelines[timeline_id].tracks[0].clips.push(clip);
    let original = project.timelines[timeline_id].tracks[0].clips[0].clone();

    let otio = timeline_to_otio(&project, timeline_id, None);
    let mut probe = probe_from(Vec::new());
    let imported = project_from_otio(&otio, Path::new("/"), &mut probe, None).unwrap();
    let (_, tl) = imported.project.timelines.iter().next().unwrap();
    let clip = &tl.tracks[0].clips[0];
    assert!(matches!(clip.source, ClipSource::Adjustment));
    assert_eq!(clip.effects.masks, original.effects.masks);
}

fn resolve_clip(name: &str, start: f64, duration: f64, effects: Value) -> Value {
    json!({
        "OTIO_SCHEMA": "Clip.2",
        "name": name,
        "source_range": range(start, duration, 24.0),
        "media_references": { "DEFAULT_MEDIA": {
            "OTIO_SCHEMA": "ExternalReference.1",
            "target_url": B_ROLL,
            "available_range": range(0.0, 2400.0, 24.0),
        }},
        "effects": effects,
    })
}

fn resolve_timeline(children: Value) -> Value {
    json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": "From Resolve",
        "tracks": { "OTIO_SCHEMA": "Stack.1", "children": [{
            "OTIO_SCHEMA": "Track.1",
            "kind": "Video",
            "children": children,
        }]},
    })
}

fn import_resolve(otio: &Value) -> OtioImport {
    let mut probe = probe_from(vec![(
        "/media/b roll.mov",
        meta(Rational::new(24, 1), 2400),
    )]);
    project_from_otio(otio, Path::new("/media"), &mut probe, None).unwrap()
}

/// Resolve splits a compound clip into `Stack`s of the same sequence: one
/// nested timeline, each piece on its own stretch of it.
#[test]
fn resolve_compound_clips_become_one_nested_timeline() {
    let stack = |start: f64, duration: f64| {
        json!({
            "OTIO_SCHEMA": "Stack.1",
            "name": "Fusion Clip 1",
            "source_range": range(start, duration, 24.0),
            "metadata": { "Resolve_OTIO": {
                "Sequence Fps": 24.0,
                "Sequence ID": "{f515}",
                "Sequence Type": "Fusion Clip",
            }},
            "children": [{
                "OTIO_SCHEMA": "Track.1",
                "kind": "Video",
                "children": [resolve_clip("inner", 100.0, 300.0, json!([]))],
            }],
        })
    };
    let otio = resolve_timeline(json!([stack(0.0, 200.0), stack(200.0, 100.0)]));
    let imported = import_resolve(&otio);
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);

    let project = &imported.project;
    let compounds: Vec<_> = project
        .media_pool
        .iter()
        .filter(|(_, item)| item.compound.is_some())
        .collect();
    assert_eq!(compounds.len(), 1);
    let (compound_id, item) = compounds[0];
    let nested = &project.timelines[item.compound.unwrap()];
    assert_eq!(nested.name, "Fusion Clip 1");
    assert_eq!(nested.resolution, (1280, 720), "the outer timeline's");
    assert_eq!(nested.tracks[0].clips[0].source_in(), 100);
    assert_eq!(item.meta.duration_frames, 300);

    let outer = project
        .timelines
        .values()
        .find(|t| t.name == "From Resolve")
        .unwrap();
    let pieces = &outer.tracks[0].clips;
    assert_eq!(pieces.len(), 2);
    for piece in pieces {
        assert!(matches!(piece.source, ClipSource::Media(id) if id == compound_id));
    }
    assert_eq!(
        pieces
            .iter()
            .map(|c| (c.timeline_start, c.source_in()))
            .collect::<Vec<_>>(),
        [(0, 0), (200, 200)]
    );
}

/// Resolve exports the two rectangles of a dynamic zoom on frames that
/// have nothing to do with the clip: they go on its first and last frame.
#[test]
fn a_resolve_dynamic_zoom_becomes_transform_keyframes() {
    let dynamic_zoom = json!({
        "OTIO_SCHEMA": "Effect.1",
        "name": "",
        "effect_name": "Resolve Effect",
        "metadata": { "Resolve_OTIO": {
            "Effect Name": "Dynamic Zoom",
            "Enabled": true,
            "Parameters": [
                {
                    "Parameter ID": "dynamicZoomCenter",
                    "Parameter Value": [0.0, 0.0],
                    "Default Parameter Value": [0.0, 0.0],
                    "Key Frames": {
                        "-100": { "Value": [0.0, 0.0] },
                        "900": { "Value": [0.1, -0.2] },
                    },
                },
                {
                    "Parameter ID": "dynamicZoomScale",
                    "Parameter Value": 1.0,
                    "Default Parameter Value": 1.0,
                    "Key Frames": {
                        "-100": { "Value": 1.0 },
                        "900": { "Value": 0.8 },
                    },
                },
            ],
        }},
    });
    let otio = resolve_timeline(json!([resolve_clip(
        "zoomed",
        100.0,
        48.0,
        json!([dynamic_zoom])
    )]));
    let imported = import_resolve(&otio);
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);

    let (_, tl) = imported.project.timelines.iter().next().unwrap();
    let clip = &tl.tracks[0].clips[0];
    let transform = &clip.effects.transform;
    let first = transform.value_at(clip.source_in());
    assert_eq!(first.zoom, [1.0, 1.0]);
    assert_eq!(first.position, [0.0, 0.0]);
    let last = transform.value_at(clip.source_out() - 1);
    assert_eq!(last.zoom, [1.25, 1.25]);
    let expected = [-0.1 * 1280.0 * 1.25, 0.2 * 720.0 * 1.25];
    for (got, want) in last.position.iter().zip(expected) {
        assert!((got - want).abs() < 1e-3, "{:?}", last.position);
    }
}

/// A freeze frame shows the frame at the start of its `source_range`, and
/// comes back the same through our metadata and through Resolve's.
#[test]
fn a_freeze_frame_imports_and_round_trips() {
    let freeze = json!({
        "OTIO_SCHEMA": "FreezeFrame.1",
        "name": "",
        "effect_name": "FreezeFrame",
        "time_scalar": 0.0,
    });
    let otio = resolve_timeline(json!([resolve_clip(
        "frozen",
        773.0,
        41.0,
        json!([freeze])
    )]));
    let imported = import_resolve(&otio);
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    let (timeline_id, tl) = imported.project.timelines.iter().next().unwrap();
    let clip = tl.tracks[0].clips[0].clone();
    assert_eq!(clip.freeze, Some(773));
    assert_eq!(clip.speed(), Rational::one());
    assert_eq!(clip.timeline_len, 41);
    assert_eq!(clip.picture_frame_at(clip.timeline_end() - 1), 773);

    let mut project = imported.project.clone();
    let clip_mut = &mut project.timelines[timeline_id].tracks[0].clips[0];
    clip_mut.source_offset = 10;
    let original = clip_mut.clone();
    let mut otio = timeline_to_otio(&project, timeline_id, None);
    let ours = import_resolve(&otio);
    let back = &ours.project.timelines.values().next().unwrap().tracks[0].clips[0];
    assert_eq!((back.freeze, back.source_offset), (Some(773), 10));
    assert_eq!(span(back), span(&original));

    for track in otio["tracks"]["children"].as_array_mut().unwrap() {
        for clip in track["children"].as_array_mut().unwrap() {
            clip["metadata"]["venturi"] = json!(null);
        }
    }
    let foreign = import_resolve(&otio);
    let back = &foreign.project.timelines.values().next().unwrap().tracks[0].clips[0];
    assert_eq!(back.freeze, Some(773));
}

#[test]
fn a_reverse_speed_still_warns() {
    let reverse = json!({
        "OTIO_SCHEMA": "LinearTimeWarp.1",
        "time_scalar": -1.0,
    });
    let otio = resolve_timeline(json!([resolve_clip("back", 100.0, 48.0, json!([reverse]))]));
    let imported = import_resolve(&otio);
    assert_eq!(
        imported.warnings,
        [OtioWarning::SpeedNotApplied {
            clip: "back".into(),
            percent: -100,
        }]
    );
    let clip = &imported.project.timelines.values().next().unwrap().tracks[0].clips[0];
    assert_eq!(clip.freeze, None);
}
