use super::*;
use crate::dispatch::tests::{clip_file, create, error, import, ok, test_dir};
use crate::{ToolCall, dispatch};

/// A session with an empty timeline (V1, A1); returns its id.
fn with_timeline(session: &mut Session) -> String {
    ok(session, ToolCall::CreateTimeline(create("T")))["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn solid(session: &mut Session, timeline: &str, at: i64, duration: i64) -> String {
    let result = ok(
        session,
        ToolCall::AddSolidColor(AddSolidColorArgs {
            timeline_id: timeline.into(),
            if_revision: None,
            at,
            duration: Some(duration),
            track: None,
            color: None,
        }),
    );
    result["clips"][0]["id"].as_str().unwrap().to_owned()
}

fn tracks(session: &mut Session, timeline: &str) -> Value {
    ok(
        session,
        ToolCall::GetTimeline(TimelineArgs {
            timeline_id: timeline.into(),
        }),
    )["tracks"]
        .clone()
}

/// `(start, end)` of the clips of a track.
fn spans(session: &mut Session, timeline: &str, track: usize) -> Vec<(i64, i64)> {
    tracks(session, timeline)[track]["clips"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["start"].as_i64().unwrap(), c["end"].as_i64().unwrap()))
        .collect()
}

fn lock(session: &mut Session, timeline: &str, track: &str) {
    ok(
        session,
        ToolCall::SetTrack(SetTrackArgs {
            timeline_id: timeline.into(),
            if_revision: None,
            track: track.into(),
            muted: None,
            solo: None,
            locked: Some(true),
        }),
    );
}

fn insert(timeline: &str, media: &str) -> InsertClipArgs {
    InsertClipArgs {
        timeline_id: timeline.into(),
        if_revision: None,
        media_id: media.into(),
        at: 0,
        source_in: None,
        source_out: None,
        video_track: None,
        audio_track: None,
        video: true,
        audio: true,
    }
}

#[test]
fn every_edit_is_one_undo_step_and_a_failed_one_leaves_none() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    let before = session.history.position();
    // A generator on a new track: two commands, one step.
    ok(
        &mut session,
        ToolCall::AddTitle(AddTitleArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            text: "Hi".into(),
            at: 0,
            duration: None,
            track: None,
            size: Some(80.0),
            color: Some([1.0, 0.0, 0.0, 1.0]),
            position: None,
        }),
    );
    assert_eq!(session.history.position(), before + 1);

    let bad = ToolCall::Split(SplitArgs {
        timeline_id: timeline.clone(),
        if_revision: None,
        frame: 1000,
        clip_ids: None,
    });
    assert_eq!(
        error(&mut session, bad),
        "no clip of an unlocked track crosses frame 1000"
    );
    assert_eq!(session.history.position(), before + 1);
    ok(&mut session, ToolCall::Undo);
    assert!(spans(&mut session, &timeline, 0).is_empty());
}

#[test]
fn insert_clip_validates_then_links_video_and_audio() {
    let dir = test_dir("insert-clip");
    let a = clip_file(&dir, "a.mp4");
    let mut session = Session::default();
    let media = import(&mut session, &[&a])["media"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let timeline = with_timeline(&mut session);

    let mut args = insert(&timeline, &media);
    args.source_out = Some(1000);
    assert!(error(&mut session, ToolCall::InsertClip(args)).contains("past the end"));
    let mut args = insert(&timeline, &media);
    args.video_track = Some("A1".into());
    assert_eq!(
        error(&mut session, ToolCall::InsertClip(args)),
        "track A1 is not a video track"
    );

    let mut args = insert(&timeline, &media);
    args.at = 10;
    args.source_in = Some(5);
    args.source_out = Some(15);
    let clips = ok(&mut session, ToolCall::InsertClip(args))["clips"].clone();
    let clips = clips.as_array().unwrap();
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0]["track"], "V1");
    assert_eq!(clips[1]["track"], "A1");
    assert_eq!(clips[0]["link_group"], clips[1]["link_group"]);
    assert!(!clips[0]["link_group"].is_null());
    assert_eq!(clips[0]["source_in"], 5);
    assert_eq!(clips[0]["source_out"], 15);
}

#[test]
fn insert_clip_audio_only_creates_its_track_when_there_is_none() {
    let dir = test_dir("insert-audio");
    let a = clip_file(&dir, "a.mp4");
    let mut session = Session::default();
    let media = import(&mut session, &[&a])["media"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let timeline = with_timeline(&mut session);
    lock(&mut session, &timeline, "A1");

    let mut args = insert(&timeline, &media);
    args.video = false;
    let clips = ok(&mut session, ToolCall::InsertClip(args))["clips"].clone();
    assert_eq!(clips[0]["track"], "A2");
}

#[test]
fn locked_tracks_refuse_edits() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    let clip = solid(&mut session, &timeline, 0, 50);
    lock(&mut session, &timeline, "V1");
    let delete = ToolCall::DeleteClips(DeleteClipsArgs {
        timeline_id: timeline.clone(),
        if_revision: None,
        clip_ids: vec![clip],
        ripple: false,
    });
    assert_eq!(error(&mut session, delete), "track V1 is locked");
    let title = ToolCall::AddTitle(AddTitleArgs {
        timeline_id: timeline.clone(),
        if_revision: None,
        text: "x".into(),
        at: 0,
        duration: None,
        track: Some("V1".into()),
        size: None,
        color: None,
        position: None,
    });
    assert_eq!(error(&mut session, title), "track V1 is locked");
}

#[test]
fn split_reports_both_halves() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    let clip = solid(&mut session, &timeline, 0, 50);

    let result = ok(
        &mut session,
        ToolCall::Split(SplitArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            frame: 20,
            clip_ids: Some(vec![clip.clone()]),
        }),
    );

    assert_eq!(result["split"][0]["left"], json!(clip));
    assert_eq!(result["split"][0]["track"], "V1");
    assert_eq!(spans(&mut session, &timeline, 0), [(0, 20), (20, 50)]);
}

#[test]
fn delete_ranges_rejects_tracks_with_ripple_and_lifts_on_named_tracks() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    solid(&mut session, &timeline, 0, 100);
    ok(
        &mut session,
        ToolCall::AddTrack(AddTrackArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            kind: TrackKindArg::Video,
        }),
    );
    let ranges = |ripple, tracks: Option<Vec<String>>| {
        ToolCall::DeleteRanges(DeleteRangesArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            ranges: vec![[10, 20]],
            ripple,
            tracks,
            media_id: None,
        })
    };
    assert!(error(&mut session, ranges(true, Some(vec!["V1".into()]))).contains("ripple"));
    assert_eq!(
        error(
            &mut session,
            ToolCall::DeleteRanges(DeleteRangesArgs {
                timeline_id: timeline.clone(),
                if_revision: None,
                ranges: vec![[20, 10]],
                ripple: false,
                tracks: None,
                media_id: None,
            })
        ),
        "invalid range [20, 10)"
    );

    ok(&mut session, ranges(false, Some(vec!["V2".into()])));
    assert_eq!(spans(&mut session, &timeline, 0), [(0, 100)]);
    ok(&mut session, ranges(true, None));
    assert_eq!(spans(&mut session, &timeline, 0), [(0, 10), (10, 90)]);
    // Named after the tool, not after the split it starts with.
    assert_eq!(ok(&mut session, ToolCall::Undo)["undone"], "RippleDelete");
}

#[test]
fn move_and_trim_validate_their_targets() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    let a = solid(&mut session, &timeline, 0, 50);
    let b = solid(&mut session, &timeline, 50, 50);

    let wrong_kind = ToolCall::MoveClips(MoveClipsArgs {
        timeline_id: timeline.clone(),
        if_revision: None,
        moves: vec![ClipMove {
            clip_id: a.clone(),
            start: 0,
            track: Some("A1".into()),
        }],
    });
    assert_eq!(
        error(&mut session, wrong_kind),
        "track A1 is not a video track"
    );
    // Moving `a` over `b` overwrites the start of `b`.
    ok(
        &mut session,
        ToolCall::MoveClips(MoveClipsArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            moves: vec![ClipMove {
                clip_id: a.clone(),
                start: 30,
                track: None,
            }],
        }),
    );
    assert_eq!(spans(&mut session, &timeline, 0), [(30, 80), (80, 100)]);

    let trim = |clip: &str, edge, frame| {
        ToolCall::TrimClip(TrimClipArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            clip_id: clip.into(),
            edge,
            frame,
        })
    };
    assert_eq!(
        error(&mut session, trim(&b, EdgeArg::Start, 100)),
        "the start edge can go from frame 0 to frame 99"
    );
    ok(&mut session, trim(&b, EdgeArg::End, 120));
    assert_eq!(spans(&mut session, &timeline, 0), [(30, 80), (80, 120)]);
}

#[test]
fn set_clip_properties_checks_ranges_and_applies_everything() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    let clip = solid(&mut session, &timeline, 0, 50);
    let props = |opacity, fade_in| SetClipPropertiesArgs {
        timeline_id: timeline.clone(),
        if_revision: None,
        clip_ids: vec![clip.clone()],
        opacity,
        position: Some([10.0, -5.0]),
        scale: None,
        rotation: None,
        gain_db: None,
        disabled: Some(true),
        fade_in,
        fade_out: None,
        fill_color: Some([0.0, 0.0, 1.0, 1.0]),
    };
    assert_eq!(
        error(
            &mut session,
            ToolCall::SetClipProperties(props(Some(150.0), None))
        ),
        "opacity goes from 0 to 100"
    );
    assert!(
        error(
            &mut session,
            ToolCall::SetClipProperties(props(None, Some(60)))
        )
        .starts_with("fade_in of clip")
    );

    let before = session.history.position();
    ok(
        &mut session,
        ToolCall::SetClipProperties(props(Some(40.0), Some(10))),
    );
    assert_eq!(session.history.position(), before + 1);
    let detail = ok(
        &mut session,
        ToolCall::GetClip(ClipArgs {
            timeline_id: timeline.clone(),
            clip_id: clip.clone(),
        }),
    );
    assert_eq!(detail["disabled"], true);
    assert_eq!(detail["fade_in"], 10);
    assert_eq!(detail["effects"]["color"]["default"]["b"], 1.0);
}

#[test]
fn links_and_markers() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    let a = solid(&mut session, &timeline, 0, 50);
    let b = solid(&mut session, &timeline, 50, 50);
    let clips = ToolCall::LinkClips(ClipsArgs {
        timeline_id: timeline.clone(),
        if_revision: None,
        clip_ids: vec![a.clone(), b.clone()],
    });
    let linked = ok(&mut session, clips);
    assert!(!linked["clips"][0]["link_group"].is_null());
    let unlink = || {
        ToolCall::UnlinkClips(ClipsArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            clip_ids: vec![a.clone()],
        })
    };
    let unlinked = ok(&mut session, unlink());
    assert!(unlinked["clips"][0]["link_group"].is_null());
    assert_eq!(
        error(&mut session, unlink()),
        "none of these clips is linked"
    );

    let marker = ok(
        &mut session,
        ToolCall::AddMarker(AddMarkerArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            at: 12,
            duration: None,
            note: Some("check".into()),
            color: None,
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut edited = ok(
        &mut session,
        ToolCall::EditMarker(EditMarkerArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            marker_id: marker.clone(),
            at: Some(20),
            duration: Some(5),
            note: None,
            color: None,
        }),
    );
    assert!(edited["revision"].is_string());
    edited.as_object_mut().unwrap().remove("revision");
    assert_eq!(
        edited,
        json!({ "id": marker, "start": 20, "duration": 5, "note": "check", "color": "yellow" })
    );
    ok(
        &mut session,
        ToolCall::DeleteMarker(MarkerArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            marker_id: marker.clone(),
        }),
    );
    let markers = ok(
        &mut session,
        ToolCall::GetTimeline(TimelineArgs {
            timeline_id: timeline.clone(),
        }),
    )["markers"]
        .clone();
    assert_eq!(markers, json!([]));
}

#[test]
fn unknown_ids_are_reported() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    let call = ToolCall::GetClip(ClipArgs {
        timeline_id: timeline.clone(),
        clip_id: "99".into(),
    });
    assert_eq!(error(&mut session, call), "no clip \"99\" in this timeline");
    let call = ToolCall::GetTimeline(TimelineArgs {
        timeline_id: "7".into(),
    });
    assert_eq!(error(&mut session, call), "unknown timeline id \"7\"");
    assert!(matches!(
        dispatch(&mut Session::default(), ToolCall::Undo),
        crate::Dispatch::Handled(Err(_))
    ));
}

#[test]
fn clip_colors_are_set_read_and_cleared() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    let a = solid(&mut session, &timeline, 0, 50);
    let b = solid(&mut session, &timeline, 50, 50);
    let color = |clip_ids: Vec<String>, color| {
        ToolCall::SetClipColor(SetClipColorArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            clip_ids,
            color,
        })
    };

    let result = ok(
        &mut session,
        color(vec![a.clone(), b.clone()], ClipColorArg::Purple),
    );
    assert_eq!(result["clips"][0]["clip_color"], "purple");
    assert_eq!(
        tracks(&mut session, &timeline)[0]["clips"][1]["clip_color"],
        "purple"
    );
    assert_eq!(
        ok(&mut session, ToolCall::Undo)["undone"],
        "ClipDisplayColor"
    );
    ok(&mut session, ToolCall::Redo);

    ok(&mut session, color(vec![a.clone()], ClipColorArg::None));
    let clip = ok(
        &mut session,
        ToolCall::GetClip(ClipArgs {
            timeline_id: timeline.clone(),
            clip_id: a,
        }),
    );
    assert!(clip["clip_color"].is_null(), "back to the default color");

    lock(&mut session, &timeline, "V1");
    assert_eq!(
        error(&mut session, color(vec![b], ClipColorArg::Red)),
        "track V1 is locked"
    );
}

#[test]
fn markers_carry_a_color_and_are_listed() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    let add = |at, color| {
        ToolCall::AddMarker(AddMarkerArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            at,
            duration: None,
            note: None,
            color,
        })
    };
    let first = ok(&mut session, add(10, None));
    assert_eq!(first["color"], "yellow", "the UI's default");
    ok(&mut session, add(40, Some(PaletteColor::Cyan)));
    ok(
        &mut session,
        ToolCall::EditMarker(EditMarkerArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            marker_id: first["id"].as_str().unwrap().into(),
            at: None,
            duration: None,
            note: Some("retake".into()),
            color: Some(PaletteColor::Red),
        }),
    );

    let markers = ok(
        &mut session,
        ToolCall::GetMarkers(TimelineArgs {
            timeline_id: timeline.clone(),
        }),
    )["markers"]
        .clone();

    let summary: Vec<(i64, &str, &str)> = markers
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["start"].as_i64().unwrap(),
                m["color"].as_str().unwrap(),
                m["note"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(summary, [(10, "red", "retake"), (40, "cyan", "")]);
}

#[test]
fn edits_are_refused_when_the_timeline_changed_since_the_agent_read_it() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    let other = with_timeline(&mut session);
    let clip = solid(&mut session, &timeline, 0, 50);
    let read = ok(
        &mut session,
        ToolCall::GetTimeline(TimelineArgs {
            timeline_id: timeline.clone(),
        }),
    )["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let split_at = |frame, if_revision: &str| {
        ToolCall::Split(SplitArgs {
            timeline_id: timeline.clone(),
            if_revision: Some(if_revision.to_owned()),
            frame,
            clip_ids: None,
        })
    };

    // Edits elsewhere do not count.
    solid(&mut session, &other, 0, 50);
    let result = ok(&mut session, split_at(20, &read));
    let after_split = result["revision"].as_str().unwrap().to_owned();
    assert_ne!(after_split, read);

    // The user edits this timeline: the agent's stale revision is refused.
    ok(
        &mut session,
        ToolCall::SetClipColor(SetClipColorArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            clip_ids: vec![clip],
            color: ClipColorArg::Red,
        }),
    );
    let refused = error(&mut session, split_at(10, &after_split));
    assert!(
        refused.starts_with("the timeline changed since you read it"),
        "{refused}"
    );
    assert_eq!(spans(&mut session, &timeline, 0), [(0, 20), (20, 50)]);
}

#[test]
fn delete_ranges_by_media_frames_finds_the_material_after_earlier_cuts() {
    let dir = test_dir("media-ranges");
    let a = clip_file(&dir, "a.mp4");
    let mut session = Session::default();
    let media = import(&mut session, &[&a])["media"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // At the media's fps, so media and timeline frames match one to one.
    let mut args = create("T");
    args.from_media = Some(media.clone());
    let timeline = ok(&mut session, ToolCall::CreateTimeline(args))["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(
        &mut session,
        ToolCall::InsertClip(insert(&timeline, &media)),
    );
    let delete = |ranges: Vec<[i64; 2]>, media_id: Option<String>| {
        ToolCall::DeleteRanges(DeleteRangesArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            ranges,
            ripple: true,
            tracks: None,
            media_id,
        })
    };
    ok(&mut session, delete(vec![[0, 10]], None));

    let result = ok(&mut session, delete(vec![[15, 20]], Some(media.clone())));

    let removed: Vec<(String, i64, i64)> = result["removed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["track"].as_str().unwrap().to_owned(),
                r["start"].as_i64().unwrap(),
                r["end"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(removed, [("V1".into(), 5, 10), ("A1".into(), 5, 10)]);
    let not_there = error(&mut session, delete(vec![[0, 5]], Some(media)));
    assert!(not_there.starts_with("none of those frames"), "{not_there}");
}

fn transition(timeline: &str, clip: &str, edge: EdgeArg) -> SetTransitionArgs {
    SetTransitionArgs {
        timeline_id: timeline.into(),
        if_revision: None,
        clip_ids: vec![clip.into()],
        edge,
        kind: None,
        duration: None,
        direction: None,
        ease: None,
        curve: None,
    }
}

fn clip_effects(session: &mut Session, timeline: &str, clip: &str) -> Value {
    ok(
        session,
        ToolCall::GetClip(ClipArgs {
            timeline_id: timeline.into(),
            clip_id: clip.into(),
        }),
    )["effects"]
        .clone()
}

#[test]
fn transitions_get_defaults_keep_earlier_values_and_are_removed() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    let clip = solid(&mut session, &timeline, 0, 100);

    let result = ok(
        &mut session,
        ToolCall::SetTransition(transition(&timeline, &clip, EdgeArg::Start)),
    );
    assert_eq!(result["clips"][0]["effects"], json!(["transition_in"]));
    let push = &clip_effects(&mut session, &timeline, &clip)["transition_in"];
    assert_eq!(push["kind"], "Push");
    // 0.45 s at the default 25 fps.
    assert_eq!(push["duration"], 11);
    assert_eq!(push["direction"], "Right");
    assert_eq!(push["ease"], "InOut");
    assert_eq!(ok(&mut session, ToolCall::Undo)["undone"], "Transition");
    ok(&mut session, ToolCall::Redo);

    let mut args = transition(&timeline, &clip, EdgeArg::Start);
    args.direction = Some(DirectionArg::Up);
    ok(&mut session, ToolCall::SetTransition(args));
    let push = &clip_effects(&mut session, &timeline, &clip)["transition_in"];
    assert_eq!(push["direction"], "Up");
    assert_eq!(push["duration"], 11);

    let mut args = transition(&timeline, &clip, EdgeArg::Start);
    args.kind = Some(TransitionKindArg::None);
    ok(&mut session, ToolCall::SetTransition(args));
    assert!(clip_effects(&mut session, &timeline, &clip)["transition_in"].is_null());
}

#[test]
fn transitions_longer_than_the_clip_are_cut_with_a_warning() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    let clip = solid(&mut session, &timeline, 0, 20);
    let mut args = transition(&timeline, &clip, EdgeArg::End);
    args.duration = Some(50);
    let result = ok(&mut session, ToolCall::SetTransition(args));
    assert_eq!(result["warnings"].as_array().unwrap().len(), 1);
    assert_eq!(
        clip_effects(&mut session, &timeline, &clip)["transition_out"]["duration"],
        20
    );

    let mut args = transition(&timeline, &clip, EdgeArg::End);
    args.curve = Some(2.0);
    assert_eq!(
        error(&mut session, ToolCall::SetTransition(args)),
        "`curve` must be between 0 and 1"
    );
}

fn mask(shape: MaskShapeArg) -> MaskArg {
    MaskArg {
        shape,
        invert: false,
        mode: None,
        center: None,
        size: None,
        rotation: None,
        roundness: None,
        feather: None,
        expansion: None,
        opacity: None,
        points: None,
    }
}

#[test]
fn masks_are_set_reported_and_validated() {
    let mut session = Session::default();
    let timeline = with_timeline(&mut session);
    let clip = solid(&mut session, &timeline, 0, 50);
    let set = |masks| {
        ToolCall::SetClipMasks(SetClipMasksArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            clip_id: clip.clone(),
            masks,
        })
    };

    let result = ok(
        &mut session,
        set(vec![
            MaskArg {
                center: Some([100.0, -50.0]),
                size: Some([300.0, 200.0]),
                feather: Some(20.0),
                invert: true,
                ..mask(MaskShapeArg::Ellipse)
            },
            MaskArg {
                mode: Some(MaskModeArg::Subtract),
                points: Some(vec![[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]]),
                ..mask(MaskShapeArg::Path)
            },
        ]),
    );
    assert!(
        result["clips"][0]["effects"]
            .as_array()
            .unwrap()
            .contains(&json!("masks"))
    );
    let masks = &clip_effects(&mut session, &timeline, &clip)["masks"];
    assert_eq!(masks[0]["shape"], "Ellipse");
    assert_eq!(masks[0]["invert"], true);
    assert_eq!(masks[1]["mode"], "Subtract");
    assert_eq!(
        masks[1]["path"]["default"]["points"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(ok(&mut session, ToolCall::Undo)["undone"], "Masks");
    ok(&mut session, ToolCall::Redo);

    assert!(error(&mut session, set(vec![mask(MaskShapeArg::Path)])).contains("points"));
    assert!(
        error(
            &mut session,
            set(vec![MaskArg {
                opacity: Some(150.0),
                ..mask(MaskShapeArg::Rectangle)
            }])
        )
        .contains("opacity")
    );
    ok(&mut session, set(Vec::new()));
    assert_eq!(
        clip_effects(&mut session, &timeline, &clip)["masks"],
        json!([])
    );
}
