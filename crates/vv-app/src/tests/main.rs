use super::*;

#[test]
fn a_new_timeline_becomes_current_and_shows_up_in_the_media_pool() {
    let mut app = VenturiApp::default();
    let first = app.ensure_timeline();
    let second = app.create_timeline(
        app.session.project.alloc_timeline_name(),
        vv_core::Rational::new(30, 1),
        (1280, 720),
    );
    assert_ne!(first, second);
    assert_eq!(app.session.project.timelines[second].name, "Timeline 2");
    app.open_timeline(second);
    assert_eq!(app.timeline_id, Some(second));
    assert!(app.timeline_stack.is_empty());
    let pool_entry = app
        .session
        .project
        .media_pool
        .values()
        .find(|item| item.compound == Some(second))
        .expect("the new timeline shows up in the media pool");
    assert_eq!(pool_entry.meta.fps, vv_core::Rational::new(30, 1));
    assert_eq!((pool_entry.meta.width, pool_entry.meta.height), (1280, 720));
    assert!(app.has_unsaved_changes());
}

#[test]
fn switching_between_project_timelines_does_not_nest_them() {
    let mut app = VenturiApp::default();
    let first = app.ensure_timeline();
    let second = app.create_timeline(
        app.session.project.alloc_timeline_name(),
        vv_core::Rational::new(30, 1),
        (1280, 720),
    );
    app.open_timeline(second);
    app.open_timeline(first);
    assert_eq!(app.timeline_id, Some(first));
    assert!(app.timeline_stack.is_empty());
}

fn make_timeline_with_clip(
    app: &mut VenturiApp,
    track_index: usize,
    start: FrameIdx,
    len: FrameIdx,
) -> vv_core::ClipId {
    if app.timeline_id.is_none() {
        let id = app.session.project.timelines.insert(vv_core::Timeline {
            name: "T".into(),
            fps: vv_core::Rational::new(25, 1),
            resolution: (1920, 1080),
            tracks: vec![Track::new(TrackKind::Video), Track::new(TrackKind::Audio)],
            markers: Vec::new(),
            master: Default::default(),
        });
        app.timeline_id = Some(id);
    }
    let clip_id = app.session.project.alloc_clip_id();
    let clip = vv_core::Clip::from_source_range(
        clip_id,
        vv_core::ClipSource::SolidColor,
        0,
        len,
        start,
        vv_core::Rational::one(),
    );
    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::InsertClip {
            timeline: app.timeline_id.unwrap(),
            track_index,
            clip,
        }),
    );
    clip_id
}

fn insert_compound_media(app: &mut VenturiApp, nested_id: TimelineId) -> MediaId {
    app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "Compound Clip 1".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 10,
            fps: vv_core::Rational::new(25, 1),
            width: 1920,
            height: 1080,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        content_hash: 1,
        compound: Some(nested_id),
        folder: None,
    })
}

/// Copying from a compound clip with video tracks only and pasting into the
/// timeline: V2 must stay video, not end up on the audio track that
/// has the same absolute index there.
#[test]
fn pasting_from_a_video_only_compound_keeps_the_clips_on_video_tracks() {
    let mut app = VenturiApp::default();
    let root_id = app.ensure_timeline();
    let nested_id = app.session.project.timelines.insert(vv_core::Timeline {
        name: "Nested".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video), Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    insert_compound_media(&mut app, nested_id);
    app.enter_compound_timeline(nested_id);
    for track_index in 0..2 {
        let clip_id = app.session.project.alloc_clip_id();
        let clip = vv_core::Clip::from_source_range(
            clip_id,
            vv_core::ClipSource::SolidColor,
            0,
            20,
            0,
            vv_core::Rational::one(),
        );
        app.session.project.timelines[nested_id].tracks[track_index].insert_sorted(clip);
        app.timeline_state.selected.insert((track_index, clip_id));
    }
    app.copy_selected_clips();

    app.exit_to_timeline_stack_index(0);
    app.timeline_state.playhead = 0;
    app.paste_clipboard_at_playhead();

    let tl = &app.session.project.timelines[root_id];
    let video_tracks: Vec<usize> = tl
        .tracks_of_kind(TrackKind::Video)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(video_tracks.len(), 2, "the missing V2 is created");
    for &index in &video_tracks {
        assert_eq!(tl.tracks[index].clips.len(), 1);
    }
    for (_, track) in tl.tracks_of_kind(TrackKind::Audio) {
        assert!(track.clips.is_empty(), "no video clip on the audio");
    }
}

#[test]
fn deleting_a_compound_clip_while_editing_it_goes_back_to_the_parent_timeline() {
    let mut app = VenturiApp::default();
    let root_id = app.ensure_timeline();
    let nested_id = app.session.project.timelines.insert(vv_core::Timeline {
        name: "Nested".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let media = insert_compound_media(&mut app, nested_id);
    app.enter_compound_timeline(nested_id);

    app.media_pool_state.selected.insert(media);
    app.delete_selected_media();

    assert_eq!(app.timeline_id, Some(root_id));
    assert!(app.timeline_stack.is_empty());
    assert!(!app.session.project.timelines.contains_key(nested_id));
}

/// A compound clip has no file to decode: the peaks are
/// composed from those of its nested audio clips.
#[test]
fn a_compound_waveform_is_composed_from_the_nested_clips() {
    let mut app = VenturiApp::default();
    let source = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "a.wav".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 50,
            fps: vv_core::Rational::new(25, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 48_000,
            channels: 2,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 7,
        compound: None,
        folder: None,
    });
    let nested_id = app.session.project.timelines.insert(vv_core::Timeline {
        name: "Nested".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Audio)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let clip = vv_core::Clip::from_source_range(
        app.session.project.alloc_clip_id(),
        vv_core::ClipSource::Media(source),
        0,
        25,
        0,
        vv_core::Rational::one(),
    );
    app.session.project.timelines[nested_id].tracks[0].insert_sorted(clip);
    let compound = insert_compound_media(&mut app, nested_id);
    app.session.project.media_pool[compound].meta.has_audio = true;
    app.session.project.media_pool[compound]
        .meta
        .duration_frames = 50;

    app.waveform_cache.insert(
        (7, 0),
        vv_media::Waveform {
            peaks: vec![1.0; 100],
            audio_duration_secs: 2.0,
        },
    );
    let (waveform, complete) =
        compose_compound_waveform(&app.session.project, &app.waveform_cache, compound).unwrap();

    assert!(complete);
    assert_eq!(waveform.audio_duration_secs, 2.0);
    let half = waveform.peaks.len() / 2;
    assert!(
        waveform.peaks[..half].iter().all(|&p| p > 0.0),
        "first half plays"
    );
    assert!(
        waveform.peaks[half + 1..].iter().all(|&p| p == 0.0),
        "second half silent"
    );
}

#[test]
fn entering_and_exiting_a_compound_timeline_switches_the_active_one_and_resets_ui_state() {
    let mut app = VenturiApp::default();
    let root_id = app.ensure_timeline();
    let nested_id = app.session.project.timelines.insert(vv_core::Timeline {
        name: "Nested".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    insert_compound_media(&mut app, nested_id);
    app.timeline_state.playhead = 42;
    app.timeline_state.selected = BTreeSet::from([(0, ClipId(999))]);

    app.enter_compound_timeline(nested_id);

    assert_eq!(app.timeline_id, Some(nested_id));
    assert_eq!(
        app.timeline_stack,
        vec![root_id],
        "the root stays on the stack for the breadcrumb"
    );
    assert_eq!(
        app.timeline_state.playhead, 0,
        "playhead reset when entering a new level"
    );
    assert!(
        app.timeline_state.selected.is_empty(),
        "selection cleared when entering a new level"
    );

    app.exit_to_timeline_stack_index(0);

    assert_eq!(app.timeline_id, Some(root_id));
    assert!(
        app.timeline_stack.is_empty(),
        "back at the root, the stack empties"
    );
}

#[test]
fn entering_the_current_timeline_or_an_ancestor_already_in_the_stack_is_a_no_op() {
    let mut app = VenturiApp::default();
    let root_id = app.ensure_timeline();

    // Intended or due to a residual cycle (see MAX_COMPOUND_DEPTH): it must not
    // stack the current timeline on itself.
    app.enter_compound_timeline(root_id);
    assert_eq!(app.timeline_id, Some(root_id));
    assert!(app.timeline_stack.is_empty());

    let nested_id = app.session.project.timelines.insert(vv_core::Timeline {
        name: "Nested".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    insert_compound_media(&mut app, nested_id);
    app.enter_compound_timeline(nested_id);
    assert_eq!(app.timeline_stack, vec![root_id]);

    // The root is already an ancestor on the stack: re-entering it must not
    // stack `nested_id` a second time over itself.
    app.enter_compound_timeline(root_id);
    assert_eq!(
        app.timeline_id,
        Some(nested_id),
        "stays where it was, the attempt is ignored"
    );
    assert_eq!(app.timeline_stack, vec![root_id], "the stack does not grow");
}

/// Copying in one timeline, entering a compound clip and pasting
/// there must work: the clipboard is not per-timeline.
#[test]
fn clipboard_survives_navigating_into_a_compound_timeline_and_pastes_there() {
    let mut app = VenturiApp::default();
    let root_id = app.ensure_timeline();
    let nested_id = app.session.project.timelines.insert(vv_core::Timeline {
        name: "Nested".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    insert_compound_media(&mut app, nested_id);

    app.timeline_state.clipboard = vec![timeline_ui::ClipboardEntry {
        track_kind: TrackKind::Video,
        track_number: 1,
        relative_start: 0,
        clip: vv_core::Clip::from_source_range(
            ClipId(1),
            vv_core::ClipSource::SolidColor,
            0,
            20,
            0,
            vv_core::Rational::one(),
        ),
        timeline_fps: vv_core::Rational::new(25, 1),
        link_tag: None,
    }];

    app.enter_compound_timeline(nested_id);
    assert_eq!(
        app.timeline_state.clipboard.len(),
        1,
        "the clipboard survives navigation"
    );

    app.timeline_state.playhead = 0;
    app.paste_clipboard_at_playhead();

    assert_eq!(
        app.session.project.timelines[nested_id].tracks[0]
            .clips
            .len(),
        1,
        "pasted into the nested timeline"
    );
    assert!(
        app.session.project.timelines[root_id].tracks[0]
            .clips
            .is_empty(),
        "not in the root"
    );
}

/// Bug reported by the user: dragging the entry of a timeline (the
/// project timeline, or a compound clip) from the media pool inside
/// itself must be refused, not only stopped during the
/// rendering (`MAX_COMPOUND_DEPTH` in render_ahead.rs is only the safety
/// net, it must never trigger in normal use).
#[test]
fn dropping_a_timelines_own_media_into_itself_is_refused() {
    let mut app = VenturiApp::default();
    let root_id = app.ensure_timeline();
    let root_media = app
        .session
        .project
        .media_pool
        .iter()
        .find(|(_, item)| item.compound == Some(root_id))
        .map(|(id, _)| id)
        .expect("the project timeline has an entry in the pool");

    app.add_media_to_timeline(root_media);

    assert!(
        app.session.project.timelines[root_id]
            .tracks
            .iter()
            .all(|t| t.clips.is_empty()),
        "the drop was rejected, no clip must appear"
    );
}

/// As above, but indirect: B already contains a clip referencing A,
/// dragging B inside A would close the cycle A -> B -> A.
#[test]
fn dropping_a_compound_clip_that_would_close_an_indirect_cycle_is_refused() {
    let mut app = VenturiApp::default();
    let root_id = app.ensure_timeline();
    let nested_id = app.session.project.timelines.insert(vv_core::Timeline {
        name: "Nested".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let compound_media = insert_compound_media(&mut app, nested_id);
    let root_media = app
        .session
        .project
        .media_pool
        .iter()
        .find(|(_, item)| item.compound == Some(root_id))
        .map(|(id, _)| id)
        .unwrap();
    // The nested timeline already contains a clip referencing the
    // root of the project.
    app.session.project.timelines[nested_id].tracks[0]
        .clips
        .push(vv_core::Clip::from_source_range(
            ClipId(1),
            vv_core::ClipSource::Media(root_media),
            0,
            10,
            0,
            vv_core::Rational::one(),
        ));

    // Dragging the compound clip (which leads to `nested_id`, which already
    // leads to the root) inside the root would close the cycle.
    app.add_media_to_timeline(compound_media);

    assert!(
        app.session.project.timelines[root_id].tracks[0]
            .clips
            .is_empty(),
        "the indirect drop was rejected"
    );
}

/// Two overlapping video tracks (plans/REFACTOR_PIPELINE.md B4): the second
/// (added with `AddTrack`, hence at the end of `tracks` — higher
/// than the default one) has a shorter clip at the center of the one
/// of the first. `active_video_clip_at` must see the top one where
/// there is one, and go back to the one below as soon as it ends — the same
/// rule the viewer uses to follow the playhead.
/// Timeline at 30 fps + media at 29.97: the inserted clip carries the
/// conforming `rate` and lasts on the timeline the real time of the
/// media, not its frames counted 1:1.
fn app_with_media_at(
    timeline_fps: vv_core::Rational,
    media_fps: vv_core::Rational,
    duration_frames: FrameIdx,
) -> (VenturiApp, MediaId) {
    let mut app = VenturiApp::default();
    let media_id = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/tmp/vv-conform-test.mp4".into(),
        meta: vv_core::MediaMeta {
            duration_frames,
            fps: media_fps,
            width: 320,
            height: 240,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let timeline_id = app.session.project.timelines.insert(vv_core::Timeline {
        name: "T".into(),
        fps: timeline_fps,
        resolution: (320, 240),
        tracks: vec![Track::new(TrackKind::Video), Track::new(TrackKind::Audio)],
        markers: Vec::new(),
        master: Default::default(),
    });
    app.timeline_id = Some(timeline_id);
    (app, media_id)
}

fn hd_media(app: &mut VenturiApp, hash: u64, (w, h): (u32, u32)) -> MediaId {
    app.session.project.media_pool.insert(vv_core::MediaItem {
        path: format!("/tmp/vv-{hash}.mp4").into(),
        meta: vv_core::MediaMeta {
            duration_frames: 100_000,
            fps: vv_core::Rational::new(60, 1),
            width: w,
            height: h,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        content_hash: hash,
        compound: None,
        folder: None,
    })
}

fn full_track_clip(id: u64, media_id: MediaId) -> vv_core::Clip {
    vv_core::Clip::from_source_range(
        ClipId(id),
        vv_core::ClipSource::Media(media_id),
        0,
        100_000,
        0,
        vv_core::Rational::one(),
    )
}

/// Two media playing together at 60 fps need more than the cache budget for
/// the full 3 s + 2 s window: the window must shrink to what fits, or the
/// worker saturates mid-window and one of the two tracks stays black.
#[test]
fn the_prefetch_window_shrinks_to_what_the_cache_budget_holds() {
    let mut app = VenturiApp::default();
    let a = hd_media(&mut app, 1, (1920, 1080));
    let b = hd_media(&mut app, 2, (1080, 2400));
    let mut track_a = Track::new(TrackKind::Video);
    track_a.clips.push(full_track_clip(1, a));
    let mut track_b = Track::new(TrackKind::Video);
    track_b.clips.push(full_track_clip(2, b));
    let timeline_id = app.session.project.timelines.insert(vv_core::Timeline {
        name: "T".into(),
        fps: vv_core::Rational::new(60, 1),
        resolution: (1920, 1080),
        tracks: vec![track_a, track_b],
        markers: Vec::new(),
        master: Default::default(),
    });
    app.timeline_id = Some(timeline_id);
    app.timeline_state.playhead = 6_000;

    let (ahead, behind) = app.effective_window_secs(timeline_id);

    let frame_bytes =
        vv_media::yuv420_frame_bytes(1920, 1080) + vv_media::yuv420_frame_bytes(1080, 2400);
    let needed = (ahead + behind) * 60.0 * frame_bytes as f64;
    assert!(
        needed <= app.settings.cache_budget_bytes as f64,
        "window {ahead}s+{behind}s = {needed} bytes, over the budget {}",
        app.settings.cache_budget_bytes
    );
    assert!(ahead >= MIN_LOOKAHEAD_SECS);
    assert!(behind > 0.0);
}

/// A single small media fits easily: the configured window is untouched.
#[test]
fn the_prefetch_window_is_not_shrunk_when_the_media_fits_the_budget() {
    let (mut app, media_id) = app_with_media_at(
        vv_core::Rational::new(25, 1),
        vv_core::Rational::new(25, 1),
        10_000,
    );
    let timeline_id = app.timeline_id.unwrap();
    app.session.project.timelines[timeline_id].tracks[0]
        .clips
        .push(full_track_clip(1, media_id));

    let (ahead, behind) = app.effective_window_secs(timeline_id);

    assert_eq!(ahead, app.settings.lookahead_secs);
    assert_eq!(behind, app.settings.behind_secs);
}

#[test]
fn dropping_several_media_appends_them_in_pool_order() {
    let (mut app, media_a) = app_with_media_at(
        vv_core::Rational::new(25, 1),
        vv_core::Rational::new(25, 1),
        50,
    );
    let media_b = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/tmp/vv-b.mp4".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 30,
            fps: vv_core::Rational::new(25, 1),
            width: 320,
            height: 240,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        content_hash: 2,
        compound: None,
        folder: None,
    });
    let timeline_id = app.timeline_id.unwrap();
    let drag = |app: &VenturiApp, id: MediaId| {
        timeline_ui::MediaDrag::whole(id, &app.session.project.media_pool[id].meta)
    };
    let set = timeline_ui::MediaDragSet {
        items: vec![drag(&app, media_a), drag(&app, media_b)],
    };

    app.add_media_set_to_timeline_at(&set, 100, timeline_ui::MediaDropTarget::Default);

    let clips = &app.session.project.timelines[timeline_id].tracks[0].clips;
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0].timeline_start, 100);
    assert_eq!(clips[0].timeline_len, 50);
    assert_eq!(
        clips[1].timeline_start, 150,
        "the second media starts where the first ends"
    );
    assert_eq!(clips[1].timeline_len, 30);
    assert!(matches!(clips[0].source, vv_core::ClipSource::Media(id) if id == media_a));
    assert!(matches!(clips[1].source, vv_core::ClipSource::Media(id) if id == media_b));
}

/// One drop = one Ctrl+Z, even with several media, several audio streams and
/// a track created on the fly.
#[test]
fn dropping_several_media_is_a_single_undo_step() {
    let (mut app, media_a) = app_with_media_at(
        vv_core::Rational::new(25, 1),
        vv_core::Rational::new(25, 1),
        50,
    );
    let media_b = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/tmp/vv-b.mp4".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 30,
            fps: vv_core::Rational::new(25, 1),
            width: 320,
            height: 240,
            has_video: true,
            has_audio: true,
            sample_rate: 48000,
            channels: 2,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 2,
        compound: None,
        folder: None,
    });
    let timeline_id = app.timeline_id.unwrap();
    let tracks_before = app.session.project.timelines[timeline_id].tracks.len();
    let drag = |app: &VenturiApp, id: MediaId| {
        timeline_ui::MediaDrag::whole(id, &app.session.project.media_pool[id].meta)
    };
    let set = timeline_ui::MediaDragSet {
        items: vec![drag(&app, media_a), drag(&app, media_b)],
    };

    app.add_media_set_to_timeline_at(&set, 0, timeline_ui::MediaDropTarget::NewVideoTrack);
    let clips_after_drop: usize = app.session.project.timelines[timeline_id]
        .tracks
        .iter()
        .map(|t| t.clips.len())
        .sum();
    assert!(clips_after_drop >= 3, "video + audio of both media");

    app.session.history.undo(&mut app.session.project);

    let tl = &app.session.project.timelines[timeline_id];
    assert!(
        tl.tracks.iter().all(|t| t.clips.is_empty()),
        "a single Ctrl+Z must remove all the clips of the drop"
    );
    assert_eq!(
        tl.tracks.len(),
        tracks_before,
        "and the track created by the drop too"
    );
}

/// Multiple drop on the "new video track" band: the track is created once
/// for the whole drop, not one per media.
#[test]
fn dropping_several_media_on_the_new_track_zone_creates_one_track() {
    let (mut app, media_a) = app_with_media_at(
        vv_core::Rational::new(25, 1),
        vv_core::Rational::new(25, 1),
        50,
    );
    let media_b = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/tmp/vv-b.mp4".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 30,
            fps: vv_core::Rational::new(25, 1),
            width: 320,
            height: 240,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        content_hash: 2,
        compound: None,
        folder: None,
    });
    let timeline_id = app.timeline_id.unwrap();
    let tracks_before = app.session.project.timelines[timeline_id].tracks.len();
    let drag = |app: &VenturiApp, id: MediaId| {
        timeline_ui::MediaDrag::whole(id, &app.session.project.media_pool[id].meta)
    };
    let set = timeline_ui::MediaDragSet {
        items: vec![drag(&app, media_a), drag(&app, media_b)],
    };

    app.add_media_set_to_timeline_at(&set, 0, timeline_ui::MediaDropTarget::NewVideoTrack);

    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks.len(), tracks_before + 1);
    assert_eq!(tl.tracks[tracks_before].clips.len(), 2);
}

#[test]
fn deleting_a_media_leaves_its_clip_in_timeline_but_offline() {
    let (mut app, media_id) = app_with_media_at(
        vv_core::Rational::new(25, 1),
        vv_core::Rational::new(25, 1),
        100,
    );
    let timeline_id = app.timeline_id.unwrap();
    let meta = app.session.project.media_pool[media_id].meta.clone();
    app.add_media_to_timeline_at(
        timeline_ui::MediaDrag::whole(media_id, &meta),
        0,
        timeline_ui::MediaDropTarget::Default,
    );
    let clip_id = app.session.project.timelines[timeline_id].tracks[0].clips[0].id;
    app.active_clip = Some((0, clip_id));

    app.media_pool_state.selected = BTreeSet::from([media_id]);
    app.delete_selected_media();

    assert!(app.session.project.media_pool.is_empty());
    assert_eq!(
        app.session.project.timelines[timeline_id].tracks[0]
            .clips
            .len(),
        1
    );
    assert!(app.active_clip_media_offline());
    assert!(app.media_pool_state.selected.is_empty());

    app.session.history.undo(&mut app.session.project);
    assert!(
        !app.active_clip_media_offline(),
        "undo must reattach the clip to the reinserted media"
    );
}

#[test]
fn a_clip_whose_file_is_missing_shows_as_offline() {
    let (mut app, media_id) = app_with_media_at(
        vv_core::Rational::new(25, 1),
        vv_core::Rational::new(25, 1),
        100,
    );
    let meta = app.session.project.media_pool[media_id].meta.clone();
    app.add_media_to_timeline_at(
        timeline_ui::MediaDrag::whole(media_id, &meta),
        0,
        timeline_ui::MediaDropTarget::Default,
    );
    let clip_id = app.session.project.timelines[app.timeline_id.unwrap()].tracks[0].clips[0].id;
    app.active_clip = Some((0, clip_id));

    app.session.project.media_pool[media_id].path = "/missing/clip.mov".into();
    app.media_pool_state.refresh_offline(&app.session.project);

    assert!(app.active_clip_media_offline());
}

#[test]
fn select_all_media_selects_every_media_in_the_pool() {
    let (mut app, media_a) = app_with_media_at(
        vv_core::Rational::new(25, 1),
        vv_core::Rational::new(25, 1),
        50,
    );
    let media_b = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/tmp/vv-b.mp4".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 30,
            fps: vv_core::Rational::new(25, 1),
            width: 320,
            height: 240,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        content_hash: 2,
        compound: None,
        folder: None,
    });

    app.select_all_media();

    assert_eq!(
        app.media_pool_state.selected,
        BTreeSet::from([media_a, media_b])
    );
}

/// Simulates moving workstation: the media points at a path that
/// no longer exists, but under a new base directory there is a file with
/// the same name, in some subdirectory.
#[test]
fn relink_media_finds_offline_files_by_name_under_the_base_folder() {
    let dir = std::env::temp_dir().join(format!("vv-app-relink-test-{}", std::process::id()));
    let nested = dir.join("project").join("clip");
    std::fs::create_dir_all(&nested).unwrap();
    let found_path = nested.join("interview.mp4");
    std::fs::write(&found_path, b"video content").unwrap();
    let already_ok_path = dir.join("already-reachable.mp4");
    std::fs::write(&already_ok_path, b"other content").unwrap();

    let mut app = VenturiApp::default();
    let meta = vv_core::MediaMeta {
        duration_frames: 10,
        fps: vv_core::Rational::new(25, 1),
        width: 320,
        height: 240,
        has_video: true,
        has_audio: false,
        sample_rate: 0,
        channels: 0,
        audio_streams: 0,
        file: Default::default(),
    };
    let offline = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/this/path/no/longer/exists/interview.mp4".into(),
        meta: meta.clone(),
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let unresolvable = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/other/nonexistent/path/ghost.mp4".into(),
        meta: meta.clone(),
        content_hash: 3,
        compound: None,
        folder: None,
    });
    let already_ok = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: already_ok_path.clone(),
        meta,
        content_hash: 2,
        compound: None,
        folder: None,
    });
    app.relink_media(&dir, &[offline, unresolvable, already_ok]);
    app.wait_for_relink();

    assert_eq!(app.session.project.media_pool[offline].path, found_path);
    assert_ne!(
        app.session.project.media_pool[offline].content_hash, 1,
        "the hash must be recomputed on the new path"
    );
    assert_eq!(
        app.session.project.media_pool[unresolvable].path,
        PathBuf::from("/other/nonexistent/path/ghost.mp4"),
        "without a matching file the path stays the old one"
    );
    assert_eq!(
        app.session.project.media_pool[already_ok].path, already_ok_path,
        "a media already reachable at its path must not be touched"
    );
    assert_eq!(app.session.project.media_pool[already_ok].content_hash, 2);
    assert_eq!(
        app.relink_message,
        Some("Relinked 1 media, 1 not found.".to_string())
    );

    app.session.history.undo(&mut app.session.project);
    assert_eq!(
        app.session.project.media_pool[offline].path,
        PathBuf::from("/this/path/no/longer/exists/interview.mp4"),
        "undo must bring the path back to the one before the relink"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The media a relink by name misses are offered to the forced relink, and
/// what it finds is applied to those alone, in one undo step.
#[test]
fn media_not_found_by_name_can_be_force_relinked() {
    let dir =
        std::env::temp_dir().join(format!("vv-app-forced-relink-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("found.mp4"), b"aa").unwrap();
    let converted = dir.join("renamed.mkv");
    std::fs::write(&converted, b"b").unwrap();

    let mut app = VenturiApp::default();
    let mut meta = vv_core::MediaMeta {
        duration_frames: 10,
        fps: vv_core::Rational::new(25, 1),
        width: 320,
        height: 240,
        has_video: true,
        has_audio: false,
        sample_rate: 0,
        channels: 0,
        audio_streams: 0,
        file: Default::default(),
    };
    let found = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/missing/found.mp4".into(),
        meta: meta.clone(),
        content_hash: 1,
        compound: None,
        folder: None,
    });
    meta.file.size_bytes = Some(1);
    let lost = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/missing/lost.mov".into(),
        meta,
        content_hash: 2,
        compound: None,
        folder: None,
    });
    app.relink_media(&dir, &[found, lost]);
    app.wait_for_relink();
    assert_eq!(app.forced_relink_offer, Some((dir.clone(), vec![lost])));

    let references = vec![forced_relink::Reference {
        media_id: lost,
        path: app.session.project.media_pool[lost].path.clone(),
        meta: app.session.project.media_pool[lost].meta.clone(),
        waveform: None,
    }];
    let criteria = forced_relink::Criteria {
        enabled: [forced_relink::Criterion::Size].into(),
        ..Default::default()
    };
    let matches = forced_relink::search(
        &references,
        &dir,
        &criteria,
        &forced_relink::SearchProgress::default(),
    )
    .unwrap();
    assert_eq!(
        matches[0].iter().map(|m| &m.path).collect::<Vec<_>>(),
        [&converted]
    );
    let relinks = matches[0]
        .iter()
        .map(|m| (lost, m.path.clone(), m.meta.clone()))
        .collect();
    app.apply_relinks(relinks);
    app.wait_for_relink();
    assert_eq!(app.session.project.media_pool[lost].path, converted);

    app.session.history.undo(&mut app.session.project);
    assert_eq!(
        app.session.project.media_pool[lost].path,
        PathBuf::from("/missing/lost.mov")
    );
    assert_eq!(
        app.session.project.media_pool[found].path,
        dir.join("found.mp4"),
        "the forced relink is its own undo step"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `relink_media` touches only the `targets` passed explicitly, never
/// the rest of the pool — even if relinkable.
#[test]
fn relink_media_with_a_selection_only_touches_the_selected_media() {
    let dir = std::env::temp_dir().join(format!(
        "vv-app-relink-selection-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let found_path = dir.join("a.mp4");
    std::fs::write(&found_path, b"a").unwrap();
    let other_found_path = dir.join("b.mp4");
    std::fs::write(&other_found_path, b"b").unwrap();

    let mut app = VenturiApp::default();
    let meta = vv_core::MediaMeta {
        duration_frames: 10,
        fps: vv_core::Rational::new(25, 1),
        width: 320,
        height: 240,
        has_video: true,
        has_audio: false,
        sample_rate: 0,
        channels: 0,
        audio_streams: 0,
        file: Default::default(),
    };
    let selected = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/missing/a.mp4".into(),
        meta: meta.clone(),
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let not_selected = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/missing/b.mp4".into(),
        meta,
        content_hash: 2,
        compound: None,
        folder: None,
    });
    app.relink_media(&dir, &[selected]);
    app.wait_for_relink();

    assert_eq!(app.session.project.media_pool[selected].path, found_path);
    assert_eq!(
        app.session.project.media_pool[not_selected].path,
        PathBuf::from("/missing/b.mp4"),
        "not selected, it is not relinked even if findable"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Only `relink_media_dialog` (the entry point from the context menu) skips
/// the file dialog without a selection: `relink_media` itself always
/// operates on the given selection, the empty one included (no target, hence
/// no command and no message).
#[test]
fn relink_media_dialog_is_a_no_op_without_a_selection() {
    let (mut app, media_id) = app_with_media_at(
        vv_core::Rational::new(25, 1),
        vv_core::Rational::new(25, 1),
        10,
    );
    let original_path = app.session.project.media_pool[media_id].path.clone();

    app.relink_media_dialog();

    assert_eq!(app.session.project.media_pool[media_id].path, original_path);
    assert!(app.relink_message.is_none());
}

#[test]
fn deleting_a_media_being_previewed_stops_the_preview() {
    let (mut app, media_id) = app_with_media_at(
        vv_core::Rational::new(25, 1),
        vv_core::Rational::new(25, 1),
        100,
    );
    app.browsing_media = Some(media_id);
    app.media_pool_state.selected = BTreeSet::from([media_id]);
    app.delete_selected_media();
    assert_eq!(app.browsing_media, None);
}

#[test]
fn inserting_a_media_at_another_fps_conforms_it_to_the_timeline() {
    let (mut app, media_id) = app_with_media_at(
        vv_core::Rational::new(30, 1),
        vv_core::Rational::new(30_000, 1001),
        3000,
    );
    let timeline_id = app.timeline_id.unwrap();
    let meta = app.session.project.media_pool[media_id].meta.clone();

    app.add_media_to_timeline_at(
        timeline_ui::MediaDrag::whole(media_id, &meta),
        0,
        timeline_ui::MediaDropTarget::Default,
    );

    let clip = &app.session.project.timelines[timeline_id].tracks[0].clips[0];
    assert_eq!(clip.rate(), vv_core::Rational::new(1001, 1000));
    assert_eq!(clip.source_len(), 3000);
    assert_eq!(clip.timeline_len, 3003, "100,1 s a 30 fps");
    assert_eq!(clip.source_frame_at(clip.timeline_end() - 1), 2999);
}

/// Lengthening the edge of a clip over the neighbor overwrites it: the
/// neighbor is cut where the new edge reaches, not moved —
/// this is what the UI does on trim release (see
/// `PendingAction::Trim`), reproduced here with the same commands.
#[test]
fn extending_a_clip_over_its_neighbor_cuts_the_neighbor() {
    let mut app = VenturiApp::default();
    let a = make_timeline_with_clip(&mut app, 0, 0, 10);
    let b = make_timeline_with_clip(&mut app, 0, 10, 10);
    let timeline_id = app.timeline_id.unwrap();

    apply_trim_with_overwrite(&mut app, timeline_id, 0, a, vv_core::TrimEdge::End, 15);

    let clips = &app.session.project.timelines[timeline_id].tracks[0].clips;
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0].id, a);
    assert_eq!(clips[0].timeline_end(), 15);
    assert_eq!(clips[1].id, b);
    assert_eq!(
        clips[1].timeline_start, 15,
        "the neighbour is cut, not moved"
    );
    assert_eq!(clips[1].timeline_end(), 20);
}

/// If the lengthening covers the neighbor entirely, the neighbor disappears.
#[test]
fn extending_a_clip_over_a_whole_neighbor_removes_it() {
    let mut app = VenturiApp::default();
    let a = make_timeline_with_clip(&mut app, 0, 0, 10);
    make_timeline_with_clip(&mut app, 0, 10, 10);
    let c = make_timeline_with_clip(&mut app, 0, 20, 10);
    let timeline_id = app.timeline_id.unwrap();

    apply_trim_with_overwrite(&mut app, timeline_id, 0, a, vv_core::TrimEdge::End, 20);

    let clips = &app.session.project.timelines[timeline_id].tracks[0].clips;
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0].id, a);
    assert_eq!(clips[0].timeline_end(), 20);
    assert_eq!(clips[1].id, c, "the fully covered clip is gone");
    assert_eq!(
        clips[1].timeline_start, 20,
        "the one after stays where it is"
    );
}

/// Lengthening the *left* edge backwards follows the same rule.
#[test]
fn extending_a_clip_backwards_cuts_the_previous_neighbor() {
    let mut app = VenturiApp::default();
    let a = make_timeline_with_clip(&mut app, 0, 0, 10);
    let timeline_id = app.timeline_id.unwrap();
    // b is born with source_in 8: it really has 8 frames of margin to
    // go back over the neighbor.
    let b = app.session.project.alloc_clip_id();
    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::InsertClip {
            timeline: timeline_id,
            track_index: 0,
            clip: vv_core::Clip::from_source_range(
                b,
                vv_core::ClipSource::SolidColor,
                8,
                18,
                12,
                vv_core::Rational::one(),
            ),
        }),
    );

    apply_trim_with_overwrite(&mut app, timeline_id, 0, b, vv_core::TrimEdge::Start, 6);

    let clips = &app.session.project.timelines[timeline_id].tracks[0].clips;
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0].id, a);
    assert_eq!(clips[0].timeline_end(), 6);
    assert_eq!(clips[1].id, b);
    assert_eq!(clips[1].timeline_start, 6);
}

/// The trim of an edge with the overwrite of what it meets, as the UI
/// composes it: first the gained stretch is freed, then the trim happens.
fn apply_trim_with_overwrite(
    app: &mut VenturiApp,
    timeline_id: TimelineId,
    track_index: usize,
    clip_id: ClipId,
    edge: vv_core::TrimEdge,
    new_value: FrameIdx,
) {
    let clip = app.session.project.timelines[timeline_id].tracks[track_index]
        .clips
        .iter()
        .find(|c| c.id == clip_id)
        .unwrap()
        .clone();
    let range = match edge {
        vv_core::TrimEdge::Start if new_value < clip.timeline_start => {
            Some((track_index, new_value, clip.timeline_start))
        }
        vv_core::TrimEdge::End if new_value > clip.timeline_end() => {
            Some((track_index, clip.timeline_end(), new_value))
        }
        _ => None,
    };
    let mut commands: Vec<Box<dyn vv_core::Command>> = Vec::new();
    vv_core::make_room_for_ranges(
        &mut app.session.project,
        timeline_id,
        range.as_slice(),
        &[(track_index, clip_id)],
        &mut commands,
    );
    commands.push(Box::new(vv_core::TrimClip::new(
        timeline_id,
        track_index,
        clip_id,
        edge,
        new_value,
    )));
    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::CompositeCommand::new(
            vv_core::CommandLabel::TrimClips,
            commands,
        )),
    );
}

/// Copy/paste of a conformed clip: the same timeline duration,
/// and the stretch freed for it (`make_room_for_ranges`) is the one it
/// will really occupy.
#[test]
fn pasting_a_conformed_clip_keeps_its_timeline_duration() {
    let (mut app, media_id) = app_with_media_at(
        vv_core::Rational::new(30, 1),
        vv_core::Rational::new(30_000, 1001),
        3000,
    );
    let timeline_id = app.timeline_id.unwrap();
    let meta = app.session.project.media_pool[media_id].meta.clone();
    app.add_media_to_timeline_at(
        timeline_ui::MediaDrag::whole(media_id, &meta),
        0,
        timeline_ui::MediaDropTarget::Default,
    );
    let clip_id = app.session.project.timelines[timeline_id].tracks[0].clips[0].id;

    app.timeline_state.selected = BTreeSet::from([(0, clip_id)]);
    app.copy_selected_clips();
    app.timeline_state.playhead = 5000;
    app.paste_clipboard_at_playhead();

    let clips = &app.session.project.timelines[timeline_id].tracks[0].clips;
    assert_eq!(clips.len(), 2);
    let pasted = clips.iter().find(|c| c.timeline_start == 5000).unwrap();
    assert_eq!(pasted.rate(), vv_core::Rational::new(1001, 1000));
    assert_eq!(pasted.timeline_len, 3003);
}

/// Pasting onto a timeline at a different fps preserves the seconds, and the
/// clip conforms to the new fps.
#[test]
fn pasting_into_a_timeline_at_another_fps_keeps_the_duration_in_seconds() {
    let (mut app, media_id) = app_with_media_at(
        vv_core::Rational::new(30, 1),
        vv_core::Rational::new(30, 1),
        300,
    );
    let timeline_id = app.timeline_id.unwrap();
    let meta = app.session.project.media_pool[media_id].meta.clone();
    app.add_media_to_timeline_at(
        timeline_ui::MediaDrag::whole(media_id, &meta),
        0,
        timeline_ui::MediaDropTarget::Default,
    );
    let clip_id = app.session.project.timelines[timeline_id].tracks[0].clips[0].id;
    app.timeline_state.selected = BTreeSet::from([(0, clip_id)]);
    app.copy_selected_clips();

    let other = app.session.project.timelines.insert(vv_core::Timeline {
        name: "T25".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (320, 240),
        tracks: vec![Track::new(TrackKind::Video), Track::new(TrackKind::Audio)],
        markers: Vec::new(),
        master: Default::default(),
    });
    app.timeline_id = Some(other);
    app.timeline_state.playhead = 50;
    app.paste_clipboard_at_playhead();

    let pasted = &app.session.project.timelines[other].tracks[0].clips[0];
    assert_eq!(pasted.timeline_start, 50);
    assert_eq!(pasted.timeline_len, 250, "10 secondi a 25 fps");
    assert_eq!(pasted.rate(), vv_core::Rational::new(5, 6));
    assert_eq!((pasted.source_in(), pasted.source_out()), (0, 300));
}

/// Overwrite of a conformed clip (pasting over its tail):
/// the cut must fall where it really falls on the timeline, not at
/// `source_in + delta` (source frames counted as timeline ones).
#[test]
fn overwriting_the_tail_of_a_conformed_clip_trims_it_at_the_right_spot() {
    let (mut app, media_id) = app_with_media_at(
        vv_core::Rational::new(30, 1),
        vv_core::Rational::new(30_000, 1001),
        3000,
    );
    let timeline_id = app.timeline_id.unwrap();
    let meta = app.session.project.media_pool[media_id].meta.clone();
    app.add_media_to_timeline_at(
        timeline_ui::MediaDrag::whole(media_id, &meta),
        0,
        timeline_ui::MediaDropTarget::Default,
    );

    let mut commands: Vec<Box<dyn vv_core::Command>> = Vec::new();
    vv_core::make_room_for_ranges(
        &mut app.session.project,
        timeline_id,
        &[(0, 2000, 4000)],
        &[],
        &mut commands,
    );
    for command in commands {
        app.session
            .history
            .do_command(&mut app.session.project, command);
    }

    let clips = &app.session.project.timelines[timeline_id].tracks[0].clips;
    assert_eq!(clips.len(), 1);
    assert_eq!(
        clips[0].timeline_end(),
        2000,
        "shortened exactly up to the freed span"
    );
    assert_eq!(clips[0].source_out(), 1998, "2000 timeline frames at 29.97");
}

#[test]
fn active_video_clip_at_prefers_the_topmost_video_track() {
    let mut app = VenturiApp::default();
    let bottom = make_timeline_with_clip(&mut app, 0, 0, 30);
    let timeline_id = app.timeline_id.unwrap();
    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::AddTrack::new(timeline_id, TrackKind::Video)),
    );
    let top = make_timeline_with_clip(&mut app, 2, 10, 10);

    assert_eq!(app.active_video_clip_at(5), Some((0, bottom)));
    assert_eq!(app.active_video_clip_at(15), Some((2, top)));
    assert_eq!(app.active_video_clip_at(25), Some((0, bottom)));
}

#[test]
fn ripple_delete_selected_shifts_other_tracks_and_selects_clip_under_playhead() {
    let mut app = VenturiApp::default();
    let video_a = make_timeline_with_clip(&mut app, 0, 0, 10);
    let video_b = make_timeline_with_clip(&mut app, 0, 10, 10);
    let _audio_a = make_timeline_with_clip(&mut app, 1, 0, 10);
    let audio_b = make_timeline_with_clip(&mut app, 1, 10, 10);
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.selected = BTreeSet::from([(0, video_b)]);
    app.ripple_delete_selected();

    // "Selection follows playhead" (on by default) reselects
    // whatever is now under the playhead (still at 0): video_a,
    // which was already there. Handy to chain several ripple-deletes without
    // having to reclick the next clip every time.
    assert_eq!(app.timeline_state.selected, BTreeSet::from([(0, video_a)]));
    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 1);
    assert_eq!(tl.tracks[0].clips[0].id, video_a);
    // The audio clip that started at the same instant moved to 0
    // even though it is on another track: global ripple behavior.
    // Arriving there it entirely covers the one that was there: the one that
    // arrives wins (see `cut_remaining_overlaps`), no stacked clips.
    assert_eq!(tl.tracks[1].clips.len(), 1);
    assert_eq!(tl.tracks[1].clips[0].id, audio_b);
    assert_eq!(tl.tracks[1].clips[0].timeline_start, 0);
}

#[test]
fn ripple_delete_selected_clears_selection_when_follow_playhead_disabled() {
    let mut app = VenturiApp {
        selection_follows_playhead: false,
        ..VenturiApp::default()
    };
    make_timeline_with_clip(&mut app, 0, 0, 10);
    let video_b = make_timeline_with_clip(&mut app, 0, 10, 10);

    app.timeline_state.selected = BTreeSet::from([(0, video_b)]);
    app.ripple_delete_selected();

    assert!(app.timeline_state.selected.is_empty());
}

/// Moving a clip over another overwrites it, like pasting one there
/// or lengthening an edge over it: it is the same rule for all the ways of
/// placing a clip (`make_room_for_ranges`).
#[test]
fn moving_a_clip_onto_another_cuts_the_one_underneath() {
    let mut app = VenturiApp::default();
    let target = make_timeline_with_clip(&mut app, 0, 0, 20);
    let moved = make_timeline_with_clip(&mut app, 1, 0, 10);
    let timeline_id = app.timeline_id.unwrap();

    let mut commands: Vec<Box<dyn vv_core::Command>> = Vec::new();
    vv_core::make_room_for_ranges(
        &mut app.session.project,
        timeline_id,
        &[(0, 10, 20)],
        &[(1, moved), (0, moved)],
        &mut commands,
    );
    commands.push(Box::new(vv_core::MoveClips::new(
        timeline_id,
        vec![(moved, 1, 0, 10)],
    )));
    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::CompositeCommand::new(
            vv_core::CommandLabel::MoveClips,
            commands,
        )),
    );

    let clips = &app.session.project.timelines[timeline_id].tracks[0].clips;
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0].id, target);
    assert_eq!(
        clips[0].timeline_end(),
        10,
        "cut where the other one arrives"
    );
    assert_eq!(clips[1].id, moved);
    assert_eq!(clips[1].timeline_start, 10);
}

/// Two *unlinked* clips covering the same stretch on different
/// tracks (the case arising from splitting at the playhead after
/// overwriting only the video part): that stretch must be closed once
/// only, not once per clip — otherwise the rest goes back twice as far and
/// ends up over what was there before.
#[test]
fn ripple_delete_of_two_unlinked_clips_on_the_same_range_closes_it_once() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);
    let video_mid = make_timeline_with_clip(&mut app, 0, 10, 10);
    let video_last = make_timeline_with_clip(&mut app, 0, 20, 10);
    make_timeline_with_clip(&mut app, 1, 0, 10);
    let audio_mid = make_timeline_with_clip(&mut app, 1, 10, 10);
    let audio_last = make_timeline_with_clip(&mut app, 1, 20, 10);
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.selected = BTreeSet::from([(0, video_mid), (1, audio_mid)]);
    app.ripple_delete_selected();

    let tl = &app.session.project.timelines[timeline_id];
    for track in 0..2 {
        assert_eq!(tl.tracks[track].clips.len(), 2);
        assert_eq!(tl.tracks[track].clips[0].timeline_start, 0);
        assert_eq!(
            tl.tracks[track].clips[1].timeline_start, 10,
            "moved back by 10, not 20"
        );
    }
    assert_eq!(tl.tracks[0].clips[1].id, video_last);
    assert_eq!(tl.tracks[1].clips[1].id, audio_last);
}

/// If a bulk move still leaves an overlap,
/// the one that arrives wins: the clip underneath is cut where the
/// other starts, not left stacked.
#[test]
fn ripple_delete_cuts_a_clip_the_shift_landed_on() {
    let mut app = VenturiApp::default();
    let video = make_timeline_with_clip(&mut app, 0, 0, 10);
    let audio_long = make_timeline_with_clip(&mut app, 1, 0, 30);
    let audio_late = make_timeline_with_clip(&mut app, 1, 30, 10);
    let timeline_id = app.timeline_id.unwrap();

    // Removing the video clip [0,10) everything goes back by 10: the long
    // audio stays where it is (it starts at 0) and the one after it ends
    // over it, from 20 instead of from 30.
    app.timeline_state.selected = BTreeSet::from([(0, video)]);
    app.ripple_delete_selected();

    let tl = &app.session.project.timelines[timeline_id];
    assert!(tl.tracks[0].clips.is_empty());
    assert_eq!(tl.tracks[1].clips.len(), 2);
    assert_eq!(tl.tracks[1].clips[0].id, audio_long);
    assert_eq!(
        tl.tracks[1].clips[0].timeline_end(),
        20,
        "cut where the one placed over it begins"
    );
    assert_eq!(tl.tracks[1].clips[1].id, audio_late);
    assert_eq!(tl.tracks[1].clips[1].timeline_start, 20);
}

/// After a ripple delete the playhead moves to where the clip that closed
/// the hole has just arrived, so the next play restarts from the
/// junction point instead of from where it was before.
#[test]
fn ripple_delete_selected_moves_playhead_to_the_clip_that_slid_back() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);
    let b = make_timeline_with_clip(&mut app, 0, 10, 10);
    let c = make_timeline_with_clip(&mut app, 0, 20, 10);
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.playhead = 25;
    app.timeline_state.selected = BTreeSet::from([(0, b)]);
    app.ripple_delete_selected();

    assert_eq!(app.timeline_state.playhead, 10);
    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips[1].id, c);
    assert_eq!(tl.tracks[0].clips[1].timeline_start, 10);
}

/// Without any clip sliding back (the last one was removed) there
/// is no junction point: the playhead must not be moved into the void.
#[test]
fn ripple_delete_selected_keeps_the_playhead_when_nothing_slides_back() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);
    let b = make_timeline_with_clip(&mut app, 0, 10, 10);

    app.timeline_state.playhead = 5;
    app.timeline_state.selected = BTreeSet::from([(0, b)]);
    app.ripple_delete_selected();

    assert_eq!(app.timeline_state.playhead, 5);
}

/// Same rule for the ripple delete of a selected gap.
#[test]
fn ripple_delete_of_a_gap_moves_playhead_to_the_closed_gap() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);
    let c = make_timeline_with_clip(&mut app, 0, 20, 10);
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.playhead = 0;
    app.timeline_state.selected_gap = Some((0, 10, 20));
    app.ripple_delete_selected();

    assert_eq!(app.timeline_state.playhead, 10);
    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips[1].id, c);
    assert_eq!(tl.tracks[0].clips[1].timeline_start, 10);
}

/// Multi-selection: deleting two non-adjacent clips together (bug
/// "I want to select several clips with ctrl+click/shift+click") must
/// close both gaps correctly, not only the first.
#[test]
fn delete_selected_removes_every_selected_clip() {
    let mut app = VenturiApp::default();
    let a = make_timeline_with_clip(&mut app, 0, 0, 10);
    let b = make_timeline_with_clip(&mut app, 0, 10, 10);
    let c = make_timeline_with_clip(&mut app, 0, 20, 10);
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.selected = BTreeSet::from([(0, a), (0, c)]);
    app.delete_selected();

    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 1);
    assert_eq!(tl.tracks[0].clips[0].id, b);
    assert_eq!(
        tl.tracks[0].clips[0].timeline_start, 10,
        "normal delete shifts nothing"
    );
}

/// Ripple-delete with two non-adjacent clips selected: processing them
/// from right to left (by decreasing `timeline_start`), every
/// removal must not alter the already computed position of the other
/// one not processed yet — otherwise one would get a double
/// move or a gap not closed correctly.
#[test]
fn ripple_delete_selected_multiple_clips_closes_every_gap() {
    let mut app = VenturiApp::default();
    let a = make_timeline_with_clip(&mut app, 0, 0, 10); // [0,10)
    let b = make_timeline_with_clip(&mut app, 0, 10, 10); // [10,20), to remove
    let c = make_timeline_with_clip(&mut app, 0, 20, 10); // [20,30)
    let d = make_timeline_with_clip(&mut app, 0, 30, 10); // [30,40), to remove
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.selected = BTreeSet::from([(0, b), (0, d)]);
    app.ripple_delete_selected();

    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 2);
    assert_eq!(tl.tracks[0].clips[0].id, a);
    assert_eq!(tl.tracks[0].clips[0].timeline_start, 0);
    assert_eq!(tl.tracks[0].clips[1].id, c);
    assert_eq!(
        tl.tracks[0].clips[1].timeline_start, 10,
        "c must slide to close the gap left by b, neither stay at 20 nor go past it"
    );

    // A single undo restores everything: both clips and the original positions.
    app.session.history.undo(&mut app.session.project);
    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 4);
    assert_eq!(tl.tracks[0].clips[2].id, c);
    assert_eq!(tl.tracks[0].clips[2].timeline_start, 20);
}

#[test]
fn delete_selected_does_not_shift_other_tracks() {
    let mut app = VenturiApp::default();
    let video_a = make_timeline_with_clip(&mut app, 0, 0, 10);
    let video_b = make_timeline_with_clip(&mut app, 0, 10, 10);
    make_timeline_with_clip(&mut app, 1, 0, 10);
    make_timeline_with_clip(&mut app, 1, 10, 10);
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.selected = BTreeSet::from([(0, video_b)]);
    app.delete_selected();

    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 1);
    assert_eq!(tl.tracks[0].clips[0].id, video_a);
    assert_eq!(tl.tracks[1].clips.len(), 2);
    assert_eq!(tl.tracks[1].clips[1].timeline_start, 10);
}

/// The media clip under the playhead becomes active and the gain set
/// via command is read from its effects.
#[test]
fn loading_a_media_clip_sets_active_clip_and_gain() {
    let dir = std::env::temp_dir().join("vv-app-main-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clip.mp4");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ],
        &path,
    );

    let mut app = VenturiApp::default();
    app.import_media(path);
    let timeline_id = app
        .timeline_id
        .expect("import should have created the timeline");
    let media_id = app
        .session
        .project
        .media_pool
        .iter()
        .find(|(_, item)| item.compound.is_none())
        .map(|(id, _)| id)
        .expect("imported media expected in the pool");

    app.add_media_to_timeline(media_id);
    let clip_id = app.session.project.timelines[timeline_id].tracks[0].clips[0].id;

    app.timeline_state.selected = BTreeSet::from([(0, clip_id)]);
    app.ensure_active_clip_matches_playhead(false);

    assert_eq!(app.active_clip, Some((0, clip_id)));
    assert_eq!(
        app.active_clip_effects().map(|e| e.gain_db.default),
        Some(0.0)
    );

    self_test_set_gain(&mut app, timeline_id, 0, clip_id, -12.0);
    assert_eq!(
        app.active_clip_effects().map(|e| e.gain_db.default),
        Some(-12.0)
    );
}

fn self_test_set_gain(
    app: &mut VenturiApp,
    timeline_id: TimelineId,
    track_index: usize,
    clip_id: vv_core::ClipId,
    db: f32,
) {
    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::set_clip_gain(
            timeline_id,
            track_index,
            clip_id,
            db,
        )),
    );
}

/// The controls of a row must not move whether the navigation arrows are
/// there or not (otherwise they dance on every playhead move).
#[test]
fn keyframe_arrow_reserves_the_same_width_when_there_is_nowhere_to_go() {
    let ctx = egui::Context::default();
    let width_with = arrow_row_width(&ctx, Some(10));
    let width_without = arrow_row_width(&ctx, None);
    assert_eq!(width_with, width_without);
}

fn arrow_row_width(ctx: &egui::Context, target: Option<FrameIdx>) -> f32 {
    let mut width = 0.0;
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                let before = ui.cursor().min.x;
                keyframe_arrow(ui, true, target, "test");
                width = ui.cursor().min.x - before;
            });
        });
    });
    output.textures_delta.clear();
    width
}

#[test]
fn keyframe_arrows_ignore_keyframes_outside_the_clip() {
    let mut app = VenturiApp::default();
    let id = make_timeline_with_clip(&mut app, 0, 0, 100);
    let timeline_id = app.timeline_id.unwrap();
    let clip = &mut app.session.project.timelines[timeline_id].tracks[0].clips[0];
    clip.source_offset = 50;
    clip.timeline_len = 50;
    let zoom = clip
        .effects
        .transform
        .track_mut(vv_core::TransformParam::ZoomX);
    zoom.upsert(10, 2.0, vv_core::Interpolation::Linear);
    zoom.upsert(40, 1.0, vv_core::Interpolation::Linear);
    let target = PanelTarget {
        timeline: timeline_id,
        track_index: 0,
        clip_id: id,
        source_frame: 70,
        timeline_start: 0,
        is_solid_color: true,
        is_text: false,
    };
    let info = app.clip_panel_info(target).unwrap();
    assert_eq!(
        info.params[vv_core::TransformParam::ZoomX.index()].prev,
        None
    );
}

/// Multiple selection: the panel builds one command per clip and
/// applies them as one, so the undo brings them back together.
#[test]
fn effect_changes_on_several_clips_are_one_undo_step() {
    let mut app = VenturiApp::default();
    let first = make_timeline_with_clip(&mut app, 0, 0, 20);
    let second = make_timeline_with_clip(&mut app, 0, 30, 20);
    let timeline_id = app.timeline_id.unwrap();

    let commands: Vec<Box<dyn vv_core::Command>> = [first, second]
        .into_iter()
        .map(|clip_id| {
            set_transform_param_default(
                (timeline_id, 0, clip_id),
                vv_core::TransformParam::PositionX,
                120.0,
            )
        })
        .collect();
    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::CompositeCommand::new(
            vv_core::CommandLabel::Transform,
            commands,
        )),
    );

    let clips = &app.session.project.timelines[timeline_id].tracks[0].clips;
    assert!(
        clips
            .iter()
            .all(|c| c.effects.transform.value_at(0).position == [120.0, 0.0]),
        "the change goes to all selected clips"
    );

    app.session.history.undo(&mut app.session.project);
    let clips = &app.session.project.timelines[timeline_id].tracks[0].clips;
    assert!(
        clips
            .iter()
            .all(|c| c.effects.transform.value_at(0).position == [0.0, 0.0]),
        "a single undo brings them all back"
    );
}

#[test]
fn editing_one_param_on_several_clips_keeps_their_other_values_and_moves_position_by_delta() {
    use vv_core::TransformParam as P;
    let mut app = VenturiApp::default();
    let first = make_timeline_with_clip(&mut app, 0, 0, 20);
    let second = make_timeline_with_clip(&mut app, 0, 30, 20);
    let timeline_id = app.timeline_id.unwrap();
    let target = |clip_id, timeline_start| PanelTarget {
        timeline: timeline_id,
        track_index: 0,
        clip_id,
        source_frame: 0,
        timeline_start,
        is_solid_color: false,
        is_text: false,
    };
    let targets = [target(first, 0), target(second, 30)];
    let cmd = set_transform_param_default((timeline_id, 0, second), P::PositionX, 50.0);
    app.session
        .history
        .do_command(&mut app.session.project, cmd);
    // The second has an animated Y: the change goes into a keyframe.
    let cmd = upsert_transform_keyframe((timeline_id, 0, second), 10, P::PositionY, 5.0);
    app.session
        .history
        .do_command(&mut app.session.project, cmd);

    let before = app.session.project.timelines[timeline_id].tracks[0].clips[0]
        .effects
        .transform
        .value_at(0);
    let mut after = before;
    after.position[1] = 80.0;
    let mut pending = Vec::new();
    push_param_changes(
        &mut pending,
        Some(&app.session.project.timelines[timeline_id]),
        &targets,
        &[P::PositionX, P::PositionY],
        &after,
        &before,
    );
    app.apply_effect_changes(pending, false);

    let clips = &app.session.project.timelines[timeline_id].tracks[0].clips;
    assert_eq!(clips[0].effects.transform.value_at(0).position, [0.0, 80.0]);
    assert_eq!(
        clips[1].effects.transform.value_at(0).position,
        [50.0, 85.0]
    );
    assert_eq!(
        clips[1].effects.transform.value_at(10).position,
        [50.0, 5.0]
    );

    // Moved again with different coordinates: the same increment to all of them.
    let before = clips[0].effects.transform.value_at(0);
    let mut after = before;
    after.position[0] += 10.0;
    let mut pending = Vec::new();
    push_param_changes(
        &mut pending,
        Some(&app.session.project.timelines[timeline_id]),
        &targets,
        &[P::PositionX, P::PositionY],
        &after,
        &before,
    );
    app.apply_effect_changes(pending, false);
    let clips = &app.session.project.timelines[timeline_id].tracks[0].clips;
    assert_eq!(
        clips[0].effects.transform.value_at(0).position,
        [10.0, 80.0]
    );
    assert_eq!(
        clips[1].effects.transform.value_at(0).position,
        [60.0, 85.0]
    );
}

#[test]
fn dragging_a_value_is_a_single_undo_step() {
    let mut app = VenturiApp::default();
    let clip_id = make_timeline_with_clip(&mut app, 0, 0, 20);
    let timeline_id = app.timeline_id.unwrap();
    let set_x = |v| {
        vec![set_transform_param_default(
            (timeline_id, 0, clip_id),
            vv_core::TransformParam::PositionX,
            v,
        )]
    };
    let x = |app: &VenturiApp| {
        app.session.project.timelines[timeline_id].tracks[0].clips[0]
            .effects
            .transform
            .value_at(0)
            .position[0]
    };

    for v in [1.0, 2.0, 3.0] {
        app.apply_effect_changes(set_x(v), true);
    }
    app.apply_effect_changes(set_x(4.0), false);
    assert_eq!(x(&app), 4.0);

    app.session.history.undo(&mut app.session.project);
    assert_eq!(x(&app), 0.0, "a single undo for the whole drag");

    app.session.history.redo(&mut app.session.project);
    app.apply_effect_changes(set_x(9.0), false);
    app.session.history.undo(&mut app.session.project);
    assert_eq!(x(&app), 4.0, "without a drag every change stands alone");
}

#[test]
fn effect_command_upsert_gain_keyframe_applies_correctly() {
    let mut app = VenturiApp::default();
    let clip_id = make_timeline_with_clip(&mut app, 0, 0, 20);
    let timeline_id = app.timeline_id.unwrap();

    let cmd = upsert_gain_keyframe((timeline_id, 0, clip_id), 5, -9.0);
    app.session
        .history
        .do_command(&mut app.session.project, cmd);

    let clip = &app.session.project.timelines[timeline_id].tracks[0].clips[0];
    assert_eq!(
        clip.effects.gain_db.keyframe_at(5),
        Some((-9.0, vv_core::Interpolation::Linear))
    );
}

#[test]
fn effect_command_remove_transform_keyframe_applies_correctly() {
    let mut app = VenturiApp::default();
    let clip_id = make_timeline_with_clip(&mut app, 0, 0, 20);
    let timeline_id = app.timeline_id.unwrap();

    app.session.history.do_command(
        &mut app.session.project,
        upsert_transform_keyframe(
            (timeline_id, 0, clip_id),
            3,
            vv_core::TransformParam::ZoomX,
            2.5,
        ),
    );
    app.session.history.do_command(
        &mut app.session.project,
        remove_transform_keyframe((timeline_id, 0, clip_id), 3, vv_core::TransformParam::ZoomX),
    );

    let clip = &app.session.project.timelines[timeline_id].tracks[0].clips[0];
    assert!(clip.effects.transform.is_constant());
}

#[test]
fn effect_command_set_defaults_applies_correctly() {
    let mut app = VenturiApp::default();
    let clip_id = make_timeline_with_clip(&mut app, 0, 0, 20);
    let timeline_id = app.timeline_id.unwrap();

    app.session.history.do_command(
        &mut app.session.project,
        set_gain_default((timeline_id, 0, clip_id), -3.0),
    );
    app.session.history.do_command(
        &mut app.session.project,
        set_transform_param_default(
            (timeline_id, 0, clip_id),
            vv_core::TransformParam::ZoomX,
            1.5,
        ),
    );

    let clip = &app.session.project.timelines[timeline_id].tracks[0].clips[0];
    assert_eq!(clip.effects.gain_db.default, -3.0);
    assert_eq!(clip.effects.transform.value_at(0).zoom, [1.5, 1.0]);
}

#[test]
fn dropping_solid_color_creates_timeline_and_initialized_color() {
    let mut app = VenturiApp::default();
    assert!(app.timeline_id.is_none());

    app.add_generator_to_timeline_at(
        timeline_ui::Generator::SolidColor,
        0,
        timeline_ui::MediaDropTarget::Default,
    );

    let timeline_id = app
        .timeline_id
        .expect("a timeline should have been created");
    let clip = &app.session.project.timelines[timeline_id].tracks[0].clips[0];
    assert!(matches!(clip.source, vv_core::ClipSource::SolidColor));
    assert!(clip.effects.color.is_some());
    assert_eq!(clip.timeline_len, 125); // 5s at 25fps by default
}

#[test]
fn dropping_solid_color_on_new_video_track_places_it_at_the_drop_frame() {
    let mut app = VenturiApp::default();
    let timeline_id = app.ensure_timeline();
    let tracks_before = app.session.project.timelines[timeline_id].tracks.len();

    app.add_generator_to_timeline_at(
        timeline_ui::Generator::SolidColor,
        50,
        timeline_ui::MediaDropTarget::NewVideoTrack,
    );

    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks.len(), tracks_before + 1);
    let clip = &tl.tracks[tracks_before].clips[0];
    assert!(matches!(clip.source, vv_core::ClipSource::SolidColor));
    assert_eq!(clip.timeline_start, 50);
    // New track and clip: a single Ctrl+Z.
    app.session.history.undo(&mut app.session.project);
    assert_eq!(
        app.session.project.timelines[timeline_id].tracks.len(),
        tracks_before
    );
}

#[test]
fn dropping_solid_color_on_a_track_overwrites_what_is_under_it() {
    let mut app = VenturiApp::default();
    let timeline_id = app.ensure_timeline();
    let drop = |app: &mut VenturiApp, start| {
        app.add_generator_to_timeline_at(
            timeline_ui::Generator::SolidColor,
            start,
            timeline_ui::MediaDropTarget::Track(0),
        )
    };
    drop(&mut app, 0); // [0, 125)
    drop(&mut app, 50); // [50, 175): cuts the tail of the first one

    let spans: Vec<(FrameIdx, FrameIdx)> = app.session.project.timelines[timeline_id].tracks[0]
        .clips
        .iter()
        .map(|c| (c.timeline_start, c.timeline_end()))
        .collect();
    assert_eq!(spans, vec![(0, 50), (50, 175)]);

    app.session.history.undo(&mut app.session.project);
    assert_eq!(
        app.session.project.timelines[timeline_id].tracks[0].clips[0].timeline_end(),
        125
    );
}

#[test]
fn dropping_text_creates_a_text_clip_with_default_title() {
    let mut app = VenturiApp::default();
    app.add_generator_to_timeline_at(
        timeline_ui::Generator::Text,
        10,
        timeline_ui::MediaDropTarget::Default,
    );
    let timeline_id = app.timeline_id.unwrap();
    let clip = &app.session.project.timelines[timeline_id].tracks[0].clips[0];
    assert!(matches!(clip.source, vv_core::ClipSource::Text));
    assert_eq!(clip.timeline_start, 10);
    assert_eq!(clip.effects.title, Some(vv_core::TitleParams::default()));
}

#[test]
fn title_edit_only_carries_the_changed_fields_to_other_clips() {
    let before = vv_core::TitleParams::default();
    let after = vv_core::TitleParams {
        size: 40.0,
        ..before.clone()
    };
    let other = vv_core::TitleParams {
        content: "Altro".into(),
        ..before.clone()
    };
    let merged = apply_title_edit(&other, &before, &after);
    assert_eq!(merged.content, "Altro");
    assert_eq!(merged.size, 40.0);

    // Inside the shadow, only the touched field.
    let mut after = before.clone();
    after.shadow.blur = 30.0;
    let mut other = before.clone();
    other.shadow.opacity = 10.0;
    let merged = apply_title_edit(&other, &before, &after);
    assert_eq!((merged.shadow.blur, merged.shadow.opacity), (30.0, 10.0));
}

#[test]
fn solid_color_clip_under_the_playhead_becomes_the_active_clip() {
    let mut app = VenturiApp::default();
    app.add_generator_to_timeline_at(
        timeline_ui::Generator::SolidColor,
        0,
        timeline_ui::MediaDropTarget::Default,
    );
    let timeline_id = app.timeline_id.unwrap();
    let clip_id = app.session.project.timelines[timeline_id].tracks[0].clips[0].id;

    app.ensure_active_clip_matches_playhead(false);

    assert_eq!(app.active_clip, Some((0, clip_id)));
}

#[test]
fn effect_command_color_upsert_and_remove_round_trip() {
    let mut app = VenturiApp::default();
    app.add_generator_to_timeline_at(
        timeline_ui::Generator::SolidColor,
        0,
        timeline_ui::MediaDropTarget::Default,
    );
    let timeline_id = app.timeline_id.unwrap();
    let clip_id = app.session.project.timelines[timeline_id].tracks[0].clips[0].id;

    let red = vv_core::Rgba {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    app.session.history.do_command(
        &mut app.session.project,
        upsert_color_keyframe((timeline_id, 0, clip_id), 10, red),
    );
    let clip = &app.session.project.timelines[timeline_id].tracks[0].clips[0];
    assert_eq!(
        clip.effects
            .color
            .as_ref()
            .unwrap()
            .keyframe_at(10)
            .unwrap()
            .0
            .r,
        1.0
    );

    app.session.history.do_command(
        &mut app.session.project,
        remove_color_keyframe((timeline_id, 0, clip_id), 10),
    );
    let clip = &app.session.project.timelines[timeline_id].tracks[0].clips[0];
    assert!(clip.effects.color.as_ref().unwrap().is_constant());
}

const TEST_FPS: f64 = 25.0;

fn clock_frame(app: &VenturiApp) -> FrameIdx {
    app.timeline_audio
        .as_ref()
        .expect("timeline_audio expected")
        .position_frame(TEST_FPS)
}

/// Simulates the mixer having reached `frame` without waiting in real time.
fn move_clock_to(app: &mut VenturiApp, frame: FrameIdx) {
    app.timeline_audio().seek_frame(frame, TEST_FPS);
}

#[test]
fn moving_the_playhead_while_paused_seeks_the_timeline_clock() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 50);
    app.timeline_audio();

    app.timeline_state.playhead = 25;
    app.ensure_active_clip_matches_playhead(false);
    assert_eq!(clock_frame(&app), 25);

    app.timeline_state.playhead = 10;
    app.ensure_active_clip_matches_playhead(false);
    assert_eq!(clock_frame(&app), 10);
}

/// Bug: "the player ignores me if I move the playhead while playing".
#[test]
fn scrubbing_during_playback_moves_the_clock_only_when_forced() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 500);
    app.toggle_playback();
    assert!(app.is_timeline_playing());

    app.timeline_state.playhead = 300;
    app.ensure_active_clip_matches_playhead(true);
    let after_scrub = clock_frame(&app);
    assert!((300..310).contains(&after_scrub), "clock={after_scrub}");

    // Playhead moved by `drive_playback`: no seek.
    move_clock_to(&mut app, 100);
    app.timeline_state.playhead = 400;
    app.ensure_active_clip_matches_playhead(false);
    assert!(clock_frame(&app) < 300);
}

#[test]
fn playback_follows_the_clock_across_a_cut() {
    let mut app = VenturiApp::default();
    let clip_a = make_timeline_with_clip(&mut app, 0, 0, 25);
    let clip_b = make_timeline_with_clip(&mut app, 0, 25, 25);
    app.toggle_playback();
    assert_eq!(app.active_clip, Some((0, clip_a)));

    move_clock_to(&mut app, 25);
    app.drive_playback();

    assert_eq!(app.timeline_state.playhead, 25);
    assert_eq!(app.active_clip, Some((0, clip_b)));
    assert!(app.is_timeline_playing());
}

/// Bug: "the selection does not follow during playback".
#[test]
fn selection_follows_playhead_during_normal_playback() {
    let mut app = VenturiApp::default();
    assert!(app.selection_follows_playhead, "on by default");
    let clip_a = make_timeline_with_clip(&mut app, 0, 0, 25);
    let clip_b = make_timeline_with_clip(&mut app, 0, 25, 25);
    app.toggle_playback();
    app.sync_selection_to_playhead();
    assert_eq!(app.timeline_state.selected, BTreeSet::from([(0, clip_a)]));

    move_clock_to(&mut app, 30);
    let playhead_before = app.timeline_state.playhead;
    app.drive_playback();
    assert_ne!(app.timeline_state.playhead, playhead_before);
    app.sync_selection_to_playhead();

    assert_eq!(app.timeline_state.selected, BTreeSet::from([(0, clip_b)]));
}

#[test]
fn the_selection_stays_on_the_graded_clip_while_the_color_window_is_open() {
    let mut app = VenturiApp::default();
    let clip_a = make_timeline_with_clip(&mut app, 0, 0, 25);
    make_timeline_with_clip(&mut app, 0, 25, 25);
    app.timeline_state.set_single_selection(Some((0, clip_a)));
    app.settings.panels.color_window_open = true;
    app.timeline_state.playhead = 30;
    app.sync_selection_to_playhead();
    assert_eq!(app.timeline_state.selected, BTreeSet::from([(0, clip_a)]));
}

/// Bug: "playback starts only if I select the clip".
#[test]
fn toggle_playback_works_without_any_selection() {
    let mut app = VenturiApp::default();
    let clip = make_timeline_with_clip(&mut app, 0, 0, 25);
    assert!(app.timeline_state.selected.is_empty());

    app.toggle_playback();

    assert!(app.is_timeline_playing());
    assert_eq!(app.active_clip, Some((0, clip)));
}

/// Bug: "when the playhead passes over an empty segment, it must not
/// jump to the next clip but play a black screen".
#[test]
fn playback_runs_through_a_gap_and_picks_up_the_next_clip() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);
    let clip_b = make_timeline_with_clip(&mut app, 0, 20, 10);
    app.toggle_playback();

    move_clock_to(&mut app, 15);
    app.drive_playback();
    assert!(app.is_timeline_playing());
    assert_eq!(app.timeline_state.playhead, 15);
    assert!(app.active_clip.is_none(), "black screen in the gap");

    move_clock_to(&mut app, 22);
    app.drive_playback();
    assert_eq!(app.active_clip, Some((0, clip_b)));
}

#[test]
fn toggle_playback_starts_from_inside_a_gap_and_pauses_again() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);
    make_timeline_with_clip(&mut app, 0, 20, 10);
    app.timeline_state.playhead = 15;

    app.toggle_playback();
    assert!(app.is_timeline_playing());
    assert_eq!(clock_frame(&app), 15);

    app.toggle_playback();
    assert!(!app.is_timeline_playing());
}

#[test]
fn toggle_playback_does_nothing_past_the_end_of_the_content() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);
    app.timeline_state.playhead = 15;

    app.toggle_playback();

    assert!(!app.is_timeline_playing());
}

/// An audio-only clip past the last video one is part of the content.
#[test]
fn playback_reaches_the_end_of_audio_only_content_and_stops_there() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);
    make_timeline_with_clip(&mut app, 1, 0, 30);
    app.timeline_state.playhead = 20;
    app.toggle_playback();
    assert!(app.is_timeline_playing());

    move_clock_to(&mut app, 45);
    app.drive_playback();

    assert!(!app.is_timeline_playing());
    assert_eq!(app.timeline_state.playhead, 30);
}

/// As `ui()` does on every frame, until the stretch reaches `speed`.
fn wait_for_playback_speed(app: &mut VenturiApp, speed: f64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.playback_speed != speed {
        assert!(
            std::time::Instant::now() < deadline,
            "speed {speed}x never applied"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        let audio = app.timeline_audio();
        audio.tick();
        app.playback_speed = audio.speed();
    }
}

/// "a" key: from stopped it starts at 1x, then 2x -> 4x -> 8x and stays at 8x; the
/// space bar always pauses and brings it back to 1x.
#[test]
fn fast_playback_key_cycles_speed_and_space_always_resets_it() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 5000);

    app.handle_fast_playback_key();
    assert!(app.is_timeline_playing());
    assert_eq!(app.playback_speed, 1.0);

    for expected in [2.0, 4.0, 8.0] {
        app.handle_fast_playback_key();
        wait_for_playback_speed(&mut app, expected);
    }
    app.handle_fast_playback_key();
    assert_eq!(app.playback_speed, 8.0, "past 8x it stays at 8x");

    app.toggle_playback();
    assert!(!app.is_timeline_playing());
    assert_eq!(app.playback_speed, 1.0, "pause restores normal speed");

    app.handle_fast_playback_key();
    assert!(app.is_timeline_playing());
    assert_eq!(app.playback_speed, 1.0);
}

#[test]
fn fast_playback_advances_the_playhead_faster() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 5000);
    app.toggle_playback();
    app.request_playback_speed(4.0);
    wait_for_playback_speed(&mut app, 4.0);
    app.drive_playback();
    let before = app.timeline_state.playhead;
    std::thread::sleep(std::time::Duration::from_millis(400));
    app.drive_playback();
    // 400ms at 4x = 1.6s = 40 frames.
    let advanced = app.timeline_state.playhead - before;
    assert!((32..=50).contains(&advanced), "advanced={advanced}");
}

/// Bug: moving/muting an audio clip changed nothing in the preview
/// (the audio always came from the media of the video clip).
#[test]
fn preview_mix_follows_audio_clip_edits() {
    let dir = std::env::temp_dir().join("vv-app-preview-mix-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clip.mp4");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x48:rate=25:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ],
        &path,
    );

    let mut app = VenturiApp::default();
    app.import_media(path);
    let media_id = app
        .session
        .project
        .media_pool
        .iter()
        .find(|(_, item)| item.compound.is_none())
        .unwrap()
        .0;
    app.add_media_to_timeline(media_id);
    let timeline_id = app.timeline_id.unwrap();
    let audio_track = app.session.project.timelines[timeline_id]
        .first_track_index(TrackKind::Audio)
        .unwrap();
    let audio_clip = app.session.project.timelines[timeline_id].tracks[audio_track].clips[0].id;

    app.timeline_audio();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        app.sync_timeline_audio();
        if !app.timeline_audio().has_pending_buffers() {
            app.sync_timeline_audio();
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "audio decode never arrived"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let peak = |app: &VenturiApp, frame| {
        app.timeline_audio
            .as_ref()
            .unwrap()
            .render(frame, TEST_FPS, 5)
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()))
    };
    assert!(peak(&app, 10) > 0.1, "the audio clip must play");
    assert_eq!(peak(&app, 60), 0.0);

    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::MoveClips::new(
            timeline_id,
            vec![(audio_clip, audio_track, audio_track, 50)],
        )),
    );
    app.sync_timeline_audio();
    assert_eq!(peak(&app, 10), 0.0, "the old position is now silent");
    assert!(peak(&app, 60) > 0.1, "the clip plays at the new position");

    // Fast forward stretches the mix, not a media.
    app.timeline_state.playhead = 50;
    app.toggle_playback();
    app.request_playback_speed(2.0);
    wait_for_playback_speed(&mut app, 2.0);
    let stretched = app.timeline_audio().stretched_peak().unwrap();
    assert!(stretched > 0.1, "stretched={stretched}");
    app.toggle_playback();

    app.session.project.timelines[timeline_id].tracks[audio_track].muted = true;
    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::MoveClips::new(
            timeline_id,
            vec![(audio_clip, audio_track, audio_track, 55)],
        )),
    );
    app.sync_timeline_audio();
    assert_eq!(peak(&app, 60), 0.0, "track muted");
}

#[test]
fn alt_released_mid_wheel_gesture_stops_zooming() {
    let ctx = egui::Context::default();
    ctx.options_mut(|o| o.input_options.zoom_modifier = egui::Modifiers::ALT);
    let wheel = |phase, modifiers| egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 2.0),
        phase,
        modifiers,
    };
    let mut zooms = Vec::new();
    for (phase, modifiers) in [
        (egui::TouchPhase::Start, egui::Modifiers::ALT),
        (egui::TouchPhase::Move, egui::Modifiers::ALT),
        (egui::TouchPhase::Move, egui::Modifiers::NONE),
    ] {
        let mut raw_input = egui::RawInput {
            events: vec![
                egui::Event::ModifiersChanged(modifiers),
                wheel(phase, modifiers),
            ],
            ..Default::default()
        };
        unstick_wheel_modifiers(&mut raw_input);
        ctx.run_ui(raw_input, |_| {}).textures_delta.clear();
        zooms.push(ctx.input(|i| i.zoom_delta()));
    }
    assert_ne!(zooms[1], 1.0, "with Alt, scrolling zooms");
    assert_eq!(
        zooms[2], 1.0,
        "with Alt released, scrolling no longer zooms"
    );
}

/// Without a selection, T cuts all the tracks in one go.
#[test]
fn split_at_playhead_cuts_every_track_without_selection() {
    let mut app = VenturiApp::default();
    let video_id = make_timeline_with_clip(&mut app, 0, 0, 20);
    let audio_id = make_timeline_with_clip(&mut app, 1, 0, 20);
    let timeline_id = app.timeline_id.unwrap();

    assert!(app.timeline_state.selected.is_empty());
    app.timeline_state.playhead = 8;
    app.split_at_playhead();

    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 2, "video track cut");
    assert_eq!(
        tl.tracks[1].clips.len(),
        2,
        "audio track cut even without selection"
    );
    assert_eq!(tl.tracks[0].clips[0].id, video_id);
    assert_eq!(tl.tracks[1].clips[0].id, audio_id);

    // A single undo cancels both cuts (CompositeCommand).
    app.session.history.undo(&mut app.session.project);
    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 1);
    assert_eq!(tl.tracks[1].clips.len(), 1);
}

fn lock_track(app: &mut VenturiApp, track_index: usize) {
    let timeline_id = app.timeline_id.unwrap();
    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::SetTrackFlag::new(
            timeline_id,
            track_index,
            vv_core::TrackFlag::Locked,
            true,
        )),
    );
}

#[test]
fn locked_tracks_are_not_split_selected_or_pasted_on() {
    let mut app = VenturiApp::default();
    let video_id = make_timeline_with_clip(&mut app, 0, 0, 20);
    make_timeline_with_clip(&mut app, 1, 0, 20);
    let timeline_id = app.timeline_id.unwrap();
    lock_track(&mut app, 1);

    app.timeline_state.playhead = 8;
    app.split_at_playhead();
    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 2);
    assert_eq!(tl.tracks[1].clips.len(), 1, "locked track untouched");

    app.select_all_clips();
    assert!(app.timeline_state.selected.iter().all(|&(t, _)| t == 0));

    app.timeline_state.clipboard = vec![timeline_ui::ClipboardEntry {
        track_kind: TrackKind::Audio,
        track_number: 1,
        relative_start: 0,
        clip: app.session.project.timelines[timeline_id].tracks[0].clips[0].clone(),
        timeline_fps: vv_core::Rational::new(25, 1),
        link_tag: None,
    }];
    app.timeline_state.playhead = 40;
    app.paste_clipboard_at_playhead();
    assert_eq!(
        app.session.project.timelines[timeline_id].tracks[1]
            .clips
            .len(),
        1
    );

    app.timeline_state
        .set_selection(BTreeSet::from([(0, video_id)]), Some((0, video_id)));
    app.ripple_delete_selected();
    assert_eq!(
        app.session.project.timelines[timeline_id].tracks[1].clips[0].timeline_start, 0,
        "ripple does not move the locked track"
    );
}

#[test]
fn d_disables_the_selection_and_enables_it_again() {
    let mut app = VenturiApp::default();
    let a = make_timeline_with_clip(&mut app, 0, 0, 20);
    let b = make_timeline_with_clip(&mut app, 0, 20, 20);
    let timeline_id = app.timeline_id.unwrap();
    let disabled = |app: &VenturiApp| -> Vec<bool> {
        app.session.project.timelines[timeline_id].tracks[0]
            .clips
            .iter()
            .map(|c| c.disabled)
            .collect()
    };

    app.timeline_state
        .set_selection(BTreeSet::from([(0, a)]), Some((0, a)));
    app.toggle_disabled_selected();
    assert_eq!(disabled(&app), vec![true, false]);

    // Mixed selection: everything gets disabled.
    app.timeline_state
        .set_selection(BTreeSet::from([(0, a), (0, b)]), Some((0, a)));
    app.toggle_disabled_selected();
    assert_eq!(disabled(&app), vec![true, true]);

    app.toggle_disabled_selected();
    assert_eq!(disabled(&app), vec![false, false]);
}

#[test]
fn dropping_media_skips_locked_tracks() {
    let (mut app, media_id) = app_with_media_at(
        vv_core::Rational::new(25, 1),
        vv_core::Rational::new(25, 1),
        50,
    );
    app.add_media_to_timeline(media_id);
    let timeline_id = app.timeline_id.unwrap();
    let video = app.session.project.timelines[timeline_id]
        .first_track_index(TrackKind::Video)
        .unwrap();
    lock_track(&mut app, video);

    let meta = app.session.project.media_pool[media_id].meta.clone();
    app.add_media_to_timeline_at(
        timeline_ui::MediaDrag::whole(media_id, &meta),
        0,
        timeline_ui::MediaDropTarget::Default,
    );
    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(
        tl.tracks[video].clips.len(),
        1,
        "nothing on the locked track"
    );
    let new_video = tl.first_unlocked_track_index(TrackKind::Video).unwrap();
    assert_ne!(new_video, video);
    assert_eq!(tl.tracks[new_video].clips.len(), 1);
}

#[test]
fn select_all_clips_takes_every_track() {
    let mut app = VenturiApp::default();
    let a = make_timeline_with_clip(&mut app, 0, 0, 20);
    let b = make_timeline_with_clip(&mut app, 1, 30, 20);
    app.select_all_clips();
    assert_eq!(
        app.timeline_state.selected,
        BTreeSet::from([(0, a), (1, b)])
    );
}

#[test]
fn select_clips_from_playhead_skips_the_ones_that_already_ended() {
    let mut app = VenturiApp::default();
    let before = make_timeline_with_clip(&mut app, 0, 0, 20);
    let under = make_timeline_with_clip(&mut app, 1, 20, 20);
    let after = make_timeline_with_clip(&mut app, 0, 50, 20);
    app.timeline_state.playhead = 25;
    app.select_clips_from_playhead();
    assert_eq!(
        app.timeline_state.selected,
        BTreeSet::from([(0, after), (1, under)])
    );
    assert!(!app.timeline_state.selected.contains(&(0, before)));
}

#[test]
fn split_at_playhead_cuts_only_selected_clips() {
    let mut app = VenturiApp::default();
    let video_id = make_timeline_with_clip(&mut app, 0, 0, 20);
    make_timeline_with_clip(&mut app, 1, 0, 20);
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.set_single_selection(Some((0, video_id)));
    app.timeline_state.playhead = 8;
    app.split_at_playhead();

    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 2, "selected clip cut");
    assert_eq!(tl.tracks[1].clips.len(), 1, "unselected clip untouched");
}

/// Bug: cutting with T a linked video+audio pair unlinked
/// both halves (correct behavior for a "single" cut,
/// but not when both members of the pair are cut
/// together at the same point): afterwards, selecting the video no longer
/// highlighted the audio. The left halves stay in the original
/// group (SplitClip does not touch it), the right halves get
/// relinked to each other in a new group.
#[test]
fn split_at_playhead_keeps_linked_group_on_both_halves() {
    let mut app = VenturiApp::default();
    let video_id = make_timeline_with_clip(&mut app, 0, 0, 20);
    let audio_id = make_timeline_with_clip(&mut app, 1, 0, 20);
    let timeline_id = app.timeline_id.unwrap();
    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::LinkClips::new(
            timeline_id,
            vec![(0, video_id), (1, audio_id)],
        )),
    );

    app.timeline_state.playhead = 8;
    app.split_at_playhead();

    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 2);
    assert_eq!(tl.tracks[1].clips.len(), 2);
    let video_left = &tl.tracks[0].clips[0];
    let video_right = &tl.tracks[0].clips[1];
    let audio_left = &tl.tracks[1].clips[0];
    let audio_right = &tl.tracks[1].clips[1];
    assert_eq!(video_left.id, video_id);
    assert_eq!(audio_left.id, audio_id);
    let left_group = video_left
        .linked_group
        .expect("the left halves stay linked");
    assert_eq!(audio_left.linked_group, Some(left_group));
    let right_group = video_right
        .linked_group
        .expect("the right halves are relinked to each other");
    assert_eq!(audio_right.linked_group, Some(right_group));
    assert_ne!(
        left_group, right_group,
        "the right halves get a new group, not the left one's"
    );
    assert_ne!(video_right.id, video_id);
    assert_ne!(audio_right.id, audio_id);

    // "Selection follows playhead" selects the LEFT half just
    // cut (the one presumed already reviewed) and its linked audio
    // twin, not the right half under the playhead.
    assert_eq!(
        app.timeline_state.selected,
        BTreeSet::from([(0, video_id), (1, audio_id)])
    );

    // A single undo cancels the cut and the relinking of the right
    // halves: the left halves were never touched, they stay in the
    // original group.
    app.session.history.undo(&mut app.session.project);
    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 1);
    assert_eq!(tl.tracks[1].clips.len(), 1);
    assert_eq!(tl.tracks[0].clips[0].linked_group, Some(left_group));
    assert_eq!(tl.tracks[1].clips[0].linked_group, Some(left_group));
}

#[test]
fn map_source_ranges_to_timeline_translates_and_clamps_to_the_trim() {
    // Clip: source_in=100, source_out=150 (a 50-frame trim), placed
    // at timeline_start=20.
    let clip = vv_core::Clip::from_source_range(
        ClipId(0),
        vv_core::ClipSource::SolidColor,
        100,
        150,
        20,
        vv_core::Rational::one(),
    );

    // Inside the trim: translated 1:1 with the offset timeline_start-source_in.
    assert_eq!(
        map_source_ranges_to_timeline(&clip, &[(110, 120)]),
        vec![(30, 40)]
    );

    // Sticking out on both sides: shortened to the trim.
    assert_eq!(
        map_source_ranges_to_timeline(&clip, &[(50, 200)]),
        vec![(20, 69)]
    );

    // Completely outside the trim: discarded.
    assert!(map_source_ranges_to_timeline(&clip, &[(0, 99)]).is_empty());

    // Several intervals: each translated/filtered independently.
    assert_eq!(
        map_source_ranges_to_timeline(&clip, &[(0, 99), (110, 115), (500, 600)]),
        vec![(30, 35)]
    );
}

#[test]
fn delete_selected_removes_every_selected_clip_together() {
    let mut app = VenturiApp::default();
    let video_id = make_timeline_with_clip(&mut app, 0, 0, 10);
    let audio_id = make_timeline_with_clip(&mut app, 1, 0, 10);
    let timeline_id = app.timeline_id.unwrap();

    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::LinkClips::new(
            timeline_id,
            vec![(0, video_id), (1, audio_id)],
        )),
    );

    // The selection already contains the whole group, as a real click
    // would make it (see `timeline_ui::expand_to_linked_groups`):
    // `delete_selected` trusts this invariant, it does not explicitly
    // pull in the links.
    app.timeline_state.selected = BTreeSet::from([(0, video_id), (1, audio_id)]);
    app.delete_selected();

    let tl = &app.session.project.timelines[timeline_id];
    assert!(tl.tracks[0].clips.is_empty());
    assert!(tl.tracks[1].clips.is_empty());
    assert!(app.timeline_state.selected.is_empty());

    // A single undo restores both (CompositeCommand).
    app.session.history.undo(&mut app.session.project);
    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 1);
    assert_eq!(tl.tracks[1].clips.len(), 1);
}

#[test]
fn arrows_step_one_frame_then_scroll_at_half_speed_while_held() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 1000);
    app.timeline_state.playhead = 100;

    app.step_playhead_with_arrows(Some(1), 10.0);
    assert_eq!(app.timeline_state.playhead, 101, "one frame right away");
    app.step_playhead_with_arrows(Some(1), 10.2);
    assert_eq!(
        app.timeline_state.playhead, 101,
        "before the delay it stays there"
    );
    // 25fps at 0.5x: 1s after the delay = 12 more frames.
    app.step_playhead_with_arrows(Some(1), 10.0 + ARROW_HOLD_DELAY_SECS + 1.0);
    assert_eq!(app.timeline_state.playhead, 113);

    app.step_playhead_with_arrows(None, 12.0);
    app.step_playhead_with_arrows(Some(-1), 12.1);
    assert_eq!(
        app.timeline_state.playhead, 112,
        "released, a new single step"
    );
}

#[test]
fn copy_then_paste_creates_a_new_clip_at_the_playhead_with_a_new_id() {
    let mut app = VenturiApp::default();
    let original_id = make_timeline_with_clip(&mut app, 0, 0, 10);
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.selected = BTreeSet::from([(0, original_id)]);
    app.copy_selected_clips();
    assert_eq!(app.timeline_state.clipboard.len(), 1);

    app.timeline_state.playhead = 50;
    app.paste_clipboard_at_playhead();

    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 2, "the original + the pasted one");
    let pasted = &tl.tracks[0].clips[1];
    assert_ne!(pasted.id, original_id, "a new id, not the same one");
    assert_eq!(pasted.timeline_start, 50, "pasted at the playhead");
    assert_eq!(pasted.timeline_len, 10);
    assert_eq!(
        app.timeline_state.selected,
        BTreeSet::from([(0, pasted.id)]),
        "the pasted clip becomes the selection"
    );
    assert_eq!(
        app.timeline_state.playhead, 60,
        "playhead at the end of the pasted clip"
    );
}

#[test]
fn copy_then_paste_relinks_a_linked_group_to_each_other_not_to_the_originals() {
    let mut app = VenturiApp::default();
    let video_id = make_timeline_with_clip(&mut app, 0, 0, 10);
    let audio_id = make_timeline_with_clip(&mut app, 1, 0, 10);
    let timeline_id = app.timeline_id.unwrap();
    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::LinkClips::new(
            timeline_id,
            vec![(0, video_id), (1, audio_id)],
        )),
    );

    // Selection already complete (as from a real click on the group, see
    // `timeline_ui::expand_to_linked_groups`).
    app.timeline_state.selected = BTreeSet::from([(0, video_id), (1, audio_id)]);
    app.copy_selected_clips();
    assert_eq!(app.timeline_state.clipboard.len(), 2);

    // Playhead moved past the originals: here only the relinking is to
    // be checked, not the "overwrite" of `make_room_for_ranges`
    // (which has a dedicated test further below).
    app.timeline_state.playhead = 100;
    app.paste_clipboard_at_playhead();

    let tl = &app.session.project.timelines[timeline_id];
    let new_video = &tl.tracks[0].clips[1];
    let new_audio = &tl.tracks[1].clips[1];
    let new_group = new_video
        .linked_group
        .expect("the pasted clips stay linked to each other");
    assert_eq!(new_audio.linked_group, Some(new_group));
    assert_ne!(
        Some(new_group),
        tl.tracks[0].clips[0].linked_group,
        "not linked to the original group"
    );
}

#[test]
fn copy_then_paste_multiple_clips_preserves_their_relative_spacing() {
    let mut app = VenturiApp::default();
    let a_id = make_timeline_with_clip(&mut app, 0, 0, 10); // [0,10)
    let b_id = make_timeline_with_clip(&mut app, 0, 20, 10); // [20,30), 10 frames of gap from "a"
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.selected = BTreeSet::from([(0, a_id), (0, b_id)]);
    app.copy_selected_clips();

    app.timeline_state.playhead = 100;
    app.paste_clipboard_at_playhead();

    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(tl.tracks[0].clips.len(), 4);
    let pasted: Vec<_> = tl.tracks[0].clips[2..].iter().collect();
    let starts: BTreeSet<FrameIdx> = pasted.iter().map(|c| c.timeline_start).collect();
    // "a" pasted at 100 (anchor = the leftmost start), "b" 20
    // frames later, exactly as in the original.
    assert_eq!(starts, BTreeSet::from([100, 120]));
}

#[test]
fn paste_with_an_empty_clipboard_is_a_no_op() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);
    let timeline_id = app.timeline_id.unwrap();

    assert!(app.timeline_state.clipboard.is_empty());
    app.paste_clipboard_at_playhead();

    assert_eq!(
        app.session.project.timelines[timeline_id].tracks[0]
            .clips
            .len(),
        1
    );
}

/// Reported bug: "copy-paste works only from the menu, not from the
/// keyboard". Cause: `egui-winit` generates `Event::Paste` only if the
/// *system* clipboard is not empty (see the docs of
/// `handle_clipboard_events`) — this test checks that an
/// `Event::Copy` always writes something non-empty there, so a
/// later Ctrl+V from the keyboard can really generate the event.
/// It runs inside a "bare" `egui::Context` (`run_ui`), not the real
/// `eframe::Frame` of `ui()` (not constructible outside `eframe`):
/// the same trick already used for `show_timeline` in `timeline_ui.rs`.
#[test]
fn handle_clipboard_events_primes_the_system_clipboard_after_a_copy() {
    let mut app = VenturiApp::default();
    let clip_id = make_timeline_with_clip(&mut app, 0, 0, 10);
    app.timeline_state.selected = BTreeSet::from([(0, clip_id)]);

    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        app.handle_clipboard_events(ui, &[egui::Event::Copy]);
    });
    output.textures_delta.clear();

    assert_eq!(app.timeline_state.clipboard.len(), 1);
    let copied_something_non_empty = output
        .platform_output
        .commands
        .iter()
        .any(|cmd| matches!(cmd, egui::OutputCommand::CopyText(text) if !text.is_empty()));
    assert!(
        copied_something_non_empty,
        "should have written something non-empty to the system clipboard"
    );
}

#[test]
fn cut_copies_the_selected_clips_and_removes_them() {
    let mut app = VenturiApp::default();
    let clip_id = make_timeline_with_clip(&mut app, 0, 0, 10);
    app.timeline_state.selected = BTreeSet::from([(0, clip_id)]);

    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        app.handle_clipboard_events(ui, &[egui::Event::Cut]);
    });
    output.textures_delta.clear();

    assert_eq!(app.timeline_state.clipboard.len(), 1);
    let timeline_id = app.timeline_id.unwrap();
    assert!(
        app.session.project.timelines[timeline_id].tracks[0]
            .clips
            .is_empty()
    );
}

/// With nothing selected, `copy_selected_clips` is a no-op: it must
/// not even touch the system clipboard (otherwise an empty Ctrl+C
/// would silently erase whatever the user might have
/// copied elsewhere to paste into another app).
#[test]
fn handle_clipboard_events_does_not_touch_system_clipboard_when_nothing_is_selected() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);

    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        app.handle_clipboard_events(ui, &[egui::Event::Copy]);
    });
    output.textures_delta.clear();

    assert!(app.timeline_state.clipboard.is_empty());
    assert!(
        output.platform_output.commands.is_empty(),
        "with nothing to copy it must not touch the system clipboard"
    );
}

/// `Event::Paste` must be handled regardless of its text payload
/// (the real clipboard is `timeline_state.clipboard`, not the system
/// text): it pastes what we had already copied anyway.
#[test]
fn handle_clipboard_events_pastes_regardless_of_the_paste_events_payload() {
    let mut app = VenturiApp::default();
    let clip_id = make_timeline_with_clip(&mut app, 0, 0, 10);
    app.timeline_state.selected = BTreeSet::from([(0, clip_id)]);
    app.copy_selected_clips();
    app.timeline_state.playhead = 100;

    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        app.handle_clipboard_events(ui, &[egui::Event::Paste("anything".to_owned())]);
    });
    output.textures_delta.clear();

    let timeline_id = app.timeline_id.unwrap();
    assert_eq!(
        app.session.project.timelines[timeline_id].tracks[0]
            .clips
            .len(),
        2
    );
}

/// Reported bug: pasting a clip over another covered it only
/// visually, but the preview kept playing the one
/// underneath. If the new interval covers an existing clip *entirely*,
/// that one must be removed altogether (`make_room_for_ranges`).
#[test]
fn paste_over_an_existing_clip_it_fully_covers_deletes_the_underlying_clip() {
    let mut app = VenturiApp::default();
    let existing = make_timeline_with_clip(&mut app, 0, 0, 10); // [0,10)
    let source = make_timeline_with_clip(&mut app, 0, 50, 10); // to copy, same length
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.selected = BTreeSet::from([(0, source)]);
    app.copy_selected_clips();
    app.timeline_state.playhead = 0;
    app.paste_clipboard_at_playhead(); // new range [0,10), covers "existing" entirely

    let tl = &app.session.project.timelines[timeline_id];
    assert!(
        tl.tracks[0].clips.iter().all(|c| c.id != existing),
        "the fully covered clip should have been removed"
    );
    // The original "source" (at 50) + the newly pasted clip (at 0).
    assert_eq!(tl.tracks[0].clips.len(), 2);
}

/// The tail of an existing clip sticks out past the start of the new
/// pasted clip: it must be shortened there (right edge), not removed nor
/// left overlapping.
#[test]
fn paste_overlapping_the_tail_of_an_existing_clip_trims_its_end() {
    let mut app = VenturiApp::default();
    let existing = make_timeline_with_clip(&mut app, 0, 0, 10); // [0,10)
    let source = make_timeline_with_clip(&mut app, 0, 50, 10);
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.selected = BTreeSet::from([(0, source)]);
    app.copy_selected_clips();
    app.timeline_state.playhead = 5;
    app.paste_clipboard_at_playhead(); // new range [5,15)

    let tl = &app.session.project.timelines[timeline_id];
    let trimmed = tl.tracks[0]
        .clips
        .iter()
        .find(|c| c.id == existing)
        .expect("it should have stayed, just shortened");
    assert_eq!(trimmed.timeline_start, 0);
    assert_eq!(trimmed.timeline_end(), 5);
}

/// The head of an existing clip sticks out before the end of the new
/// pasted clip: it must be shortened there (left edge).
#[test]
fn paste_overlapping_the_head_of_an_existing_clip_trims_its_start() {
    let mut app = VenturiApp::default();
    let existing = make_timeline_with_clip(&mut app, 0, 10, 10); // [10,20)
    let source = make_timeline_with_clip(&mut app, 0, 50, 10);
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.selected = BTreeSet::from([(0, source)]);
    app.copy_selected_clips();
    app.timeline_state.playhead = 5;
    app.paste_clipboard_at_playhead(); // new range [5,15)

    let tl = &app.session.project.timelines[timeline_id];
    let trimmed = tl.tracks[0]
        .clips
        .iter()
        .find(|c| c.id == existing)
        .expect("it should have stayed, just shortened");
    assert_eq!(trimmed.timeline_start, 15);
    assert_eq!(trimmed.timeline_end(), 20);
}

/// The new pasted clip falls entirely in the middle of a longer
/// existing clip: that one must be split in two, with the central piece
/// (covered) disappearing.
#[test]
fn paste_inside_an_existing_clip_splits_it_in_two() {
    let mut app = VenturiApp::default();
    let existing = make_timeline_with_clip(&mut app, 0, 0, 20); // [0,20)
    let source = make_timeline_with_clip(&mut app, 0, 50, 5);
    let timeline_id = app.timeline_id.unwrap();

    app.timeline_state.selected = BTreeSet::from([(0, source)]);
    app.copy_selected_clips();
    app.timeline_state.playhead = 8;
    app.paste_clipboard_at_playhead(); // new range [8,13)

    // Besides "existing" (due to split) and the clip just
    // pasted (at 8), "source" is also still around (at 50, never
    // touched: it is the source of the copy, not overlapping anything).
    // The two expected pieces are exactly at 0 and 13.
    let tl = &app.session.project.timelines[timeline_id];
    let mut halves: Vec<_> = tl.tracks[0]
        .clips
        .iter()
        .filter(|c| c.timeline_start == 0 || c.timeline_start == 13)
        .collect();
    halves.sort_by_key(|c| c.timeline_start);
    assert_eq!(
        halves.len(),
        2,
        "the original clip should have split in two"
    );
    assert_eq!(
        halves[0].id, existing,
        "the left half keeps the original id (SplitClip behaviour)"
    );
    assert_eq!(halves[0].timeline_end(), 8);
    assert_eq!(halves[1].timeline_start, 13);
    assert_eq!(halves[1].timeline_end(), 20);
}

/// If the split involves a linked group (paste of the video+audio pair
/// copied together, which therefore cuts both tracks
/// at the same point), the two new halves must stay linked
/// *to each other*, not to the old twin (lost in the split).
#[test]
fn paste_splitting_a_linked_group_relinks_the_new_halves_to_each_other() {
    let mut app = VenturiApp::default();
    let video_id = make_timeline_with_clip(&mut app, 0, 0, 20); // [0,20)
    let audio_id = make_timeline_with_clip(&mut app, 1, 0, 20); // [0,20)
    let timeline_id = app.timeline_id.unwrap();
    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::LinkClips::new(
            timeline_id,
            vec![(0, video_id), (1, audio_id)],
        )),
    );

    let src_video = make_timeline_with_clip(&mut app, 0, 100, 5);
    let src_audio = make_timeline_with_clip(&mut app, 1, 100, 5);
    app.session.history.do_command(
        &mut app.session.project,
        Box::new(vv_core::LinkClips::new(
            timeline_id,
            vec![(0, src_video), (1, src_audio)],
        )),
    );
    // Selection already complete (as from a real click on the group).
    app.timeline_state.selected = BTreeSet::from([(0, src_video), (1, src_audio)]);
    app.copy_selected_clips();
    assert_eq!(app.timeline_state.clipboard.len(), 2);

    app.timeline_state.playhead = 8;
    app.paste_clipboard_at_playhead(); // new range [8,13) on both tracks

    // Besides the two halves (at 0 and 13), "src_video"/"src_audio"
    // are also still around (at 100, never touched: they are the source
    // of the copy).
    let tl = &app.session.project.timelines[timeline_id];
    let mut video_halves: Vec<_> = tl.tracks[0]
        .clips
        .iter()
        .filter(|c| c.timeline_start == 0 || c.timeline_start == 13)
        .collect();
    video_halves.sort_by_key(|c| c.timeline_start);
    let mut audio_halves: Vec<_> = tl.tracks[1]
        .clips
        .iter()
        .filter(|c| c.timeline_start == 0 || c.timeline_start == 13)
        .collect();
    audio_halves.sort_by_key(|c| c.timeline_start);

    assert_eq!(video_halves.len(), 2);
    assert_eq!(audio_halves.len(), 2);
    let left_group = video_halves[0]
        .linked_group
        .expect("the left half stays in the original group");
    assert_eq!(audio_halves[0].linked_group, Some(left_group));
    let right_group = video_halves[1]
        .linked_group
        .expect("the right half is relinked to its twin");
    assert_eq!(audio_halves[1].linked_group, Some(right_group));
    assert_ne!(left_group, right_group);
}

/// Request: "add a visual indicator of the portions of the
/// timeline present in memory". It checks the end-to-end integration
/// (not only the pure function `map_source_ranges_to_timeline`, already
/// covered separately): with a real `render_ahead` running on a
/// separate thread, the clip under the playhead produces buffered intervals
/// within its own timeline limits.
#[test]
fn buffered_timeline_ranges_reports_the_clip_under_the_playheads_decoded_frames() {
    let dir = std::env::temp_dir().join("vv-app-buffered-ranges-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clip.mp4");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );

    let mut app = VenturiApp::default();
    app.import_media(path);
    let media_id = app
        .session
        .project
        .media_pool
        .iter()
        .find(|(_, item)| item.compound.is_none())
        .unwrap()
        .0;
    app.add_media_to_timeline(media_id);
    let timeline_id = app.timeline_id.unwrap();
    let clip = app.session.project.timelines[timeline_id].tracks[0].clips[0].clone();

    // Sends `render_ahead` the just inserted clip (in the real app this
    // happens on every UI frame via `sync_render_ahead`).
    app.sync_render_ahead();

    let start = std::time::Instant::now();
    loop {
        if !app.buffered_timeline_ranges().is_empty() {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "timeout: no frame buffered within 2s"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    for (s, e) in app.buffered_timeline_ranges() {
        assert!(
            s >= clip.timeline_start && e < clip.timeline_end(),
            "range {s}..{e} outside the clip bounds ({}..{})",
            clip.timeline_start,
            clip.timeline_end()
        );
    }
}

/// The "buffered" strip must not skip the compound clips: their
/// cached frames are those of the media of the nested timeline.
#[test]
fn buffered_timeline_ranges_covers_a_compound_clip_through_its_nested_timeline() {
    let dir = std::env::temp_dir().join("vv-app-buffered-ranges-compound-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clip.mp4");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );

    let mut app = VenturiApp::default();
    app.import_media(path);
    let media_id = app
        .session
        .project
        .media_pool
        .iter()
        .find(|(_, item)| item.compound.is_none())
        .unwrap()
        .0;
    app.add_media_to_timeline(media_id);
    let timeline_id = app.timeline_id.unwrap();

    // The imported clip becomes the content of a compound clip, and
    // on the timeline only the latter remains.
    let inner = app.session.project.timelines[timeline_id].tracks[0]
        .clips
        .remove(0);
    let len = inner.timeline_len;
    let nested_id = app.session.project.timelines.insert(vv_core::Timeline {
        name: "Nested".into(),
        fps: app.session.project.timelines[timeline_id].fps,
        resolution: (320, 240),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    app.session.project.timelines[nested_id].tracks[0].insert_sorted(inner);
    let compound = insert_compound_media(&mut app, nested_id);
    app.session.project.media_pool[compound]
        .meta
        .duration_frames = len;
    let compound_clip = vv_core::Clip::from_source_range(
        app.session.project.alloc_clip_id(),
        vv_core::ClipSource::Media(compound),
        0,
        len,
        0,
        vv_core::Rational::one(),
    );
    let (start, end) = (compound_clip.timeline_start, compound_clip.timeline_end());
    app.session.project.timelines[timeline_id].tracks[0].insert_sorted(compound_clip);
    app.sync_render_ahead();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let ranges = app.buffered_timeline_ranges();
        if !ranges.is_empty() {
            for (s, e) in ranges {
                assert!(
                    s >= start && e < end,
                    "range {s}..{e} outside the compound ({start}..{end})"
                );
            }
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timeout: the compound clip never shows as buffered"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// `proxy_timeline_ranges` covers the whole clip as soon as the proxy is
/// ready on disk (not only the part already buffered, unlike
/// `buffered_timeline_ranges` — see the docs of the method),
/// it is empty until it is, and it is empty regardless if the toggle is
/// off.
#[test]
fn proxy_timeline_ranges_covers_the_whole_clip_once_the_proxy_is_ready() {
    let dir = std::env::temp_dir().join("vv-app-proxy-ranges-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clip.mp4");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );

    // Fingerprint computed before the actual import, only to
    // clean up a proxy possibly left from an earlier run of
    // this same test with the same content_hash (path+size+
    // mtime coinciding) — otherwise the assertion "empty right after
    // the import" below would be fragile, not because of a real race but because of
    // residual state on disk.
    let content_hash = vv_media::content_fingerprint(&path).unwrap();
    let quality = vv_media::proxy::ProxyQuality::default();
    let _ = std::fs::remove_file(vv_media::proxy::proxy_path_for(content_hash, quality));

    let mut app = VenturiApp::default();
    app.settings.proxy_enabled = true;
    app.import_media(path); // it also queues the proxy generation
    let media_id = app
        .session
        .project
        .media_pool
        .iter()
        .find(|(_, item)| item.compound.is_none())
        .unwrap()
        .0;
    app.add_media_to_timeline(media_id);
    let timeline_id = app.timeline_id.unwrap();
    let clip = app.session.project.timelines[timeline_id].tracks[0].clips[0].clone();

    assert!(
        app.proxy_timeline_ranges().is_empty(),
        "the proxy cannot be ready right after the import"
    );

    // Not the file: the worker marks the proxy ready only after renaming it
    // into place.
    let start = std::time::Instant::now();
    loop {
        if !app.proxy_timeline_ranges().is_empty() {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(15),
            "timeout: proxy never generated"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert_eq!(
        app.proxy_timeline_ranges(),
        vec![(clip.timeline_start, clip.timeline_end() - 1)],
        "with the proxy ready and the toggle on, it must cover the whole clip"
    );

    app.settings.proxy_enabled = false;
    assert!(
        app.proxy_timeline_ranges().is_empty(),
        "with the toggle off it must report nothing, even with the proxy ready"
    );
}

#[test]
fn import_media_files_imports_every_file_and_queues_their_proxies() {
    let dir = std::env::temp_dir().join("vv-app-multi-import-test");
    std::fs::create_dir_all(&dir).unwrap();
    let paths: Vec<PathBuf> = ["a.mp4", "b.mp4"]
        .iter()
        .map(|name| {
            let path = dir.join(name);
            vv_media::test_support::ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=size=320x240:rate=25:duration=1",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                ],
                &path,
            );
            path
        })
        .collect();

    let mut app = VenturiApp::default();
    app.settings.proxy_enabled = true;
    let mut with_bad = paths.clone();
    with_bad.push(dir.join("nonexistent.mp4"));
    app.import_media_files(with_bad);
    app.wait_for_import();

    // +1: the project timeline shows up in the pool too.
    assert_eq!(
        app.session
            .project
            .media_pool
            .values()
            .filter(|m| m.compound.is_none())
            .count(),
        2
    );
    let warnings = &app.import_warnings;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("nonexistent.mp4"), "{warnings:?}");
    let worker = app.proxy_worker.as_ref().unwrap();
    assert_eq!(worker.progress().total, 2);
    for item in app
        .session
        .project
        .media_pool
        .values()
        .filter(|m| m.compound.is_none())
    {
        assert!(worker.state(item.content_hash).is_some());
        assert!(app.thumbnails.contains_key(&item.content_hash));
    }
}

#[test]
fn imported_media_end_up_selected_in_the_pool() {
    let dir = std::env::temp_dir().join("vv-app-import-selection-test");
    std::fs::create_dir_all(&dir).unwrap();
    let paths: Vec<PathBuf> = ["x.mp4", "y.mp4"]
        .iter()
        .map(|name| {
            let path = dir.join(name);
            vv_media::test_support::ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=size=320x240:rate=25:duration=1",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                ],
                &path,
            );
            path
        })
        .collect();

    let mut app = VenturiApp::default();
    app.import_media_files(paths);
    app.wait_for_import();

    let imported: BTreeSet<MediaId> = app
        .session
        .project
        .media_pool
        .iter()
        .filter(|(_, m)| m.compound.is_none())
        .map(|(id, _)| id)
        .collect();
    assert_eq!(app.media_pool_state.selected, imported);
}

/// Re-importing the same file does not duplicate it in the pool: the
/// media already there is selected again.
#[test]
fn importing_an_already_present_path_is_skipped() {
    let path = make_wav("already-present.wav");
    let mut app = VenturiApp::default();
    app.import_media(path.clone());
    let pool_count = |app: &VenturiApp| {
        app.session
            .project
            .media_pool
            .values()
            .filter(|m| m.compound.is_none())
            .count()
    };
    assert_eq!(pool_count(&app), 1);
    let media_id = app
        .session
        .project
        .media_pool
        .iter()
        .find(|(_, m)| m.compound.is_none())
        .map(|(id, _)| id)
        .unwrap();

    app.import_media(path.clone());
    assert_eq!(pool_count(&app), 1);
    assert_eq!(app.media_pool_state.selected, BTreeSet::from([media_id]));

    // Same file via a non-canonical path, and duplicated inside the batch.
    let detour = path
        .parent()
        .unwrap()
        .join("..")
        .join(path.parent().unwrap().file_name().unwrap())
        .join("already-present.wav");
    app.import_media_files(vec![path.clone(), detour, make_wav("new.wav")]);
    app.wait_for_import();
    assert_eq!(pool_count(&app), 2);
}

/// Minimal counterpart of `egui::DroppedFile` to simulate a
/// drag from the file manager without a real windowing
/// backend — only `path()` is needed by `poll_dropped_files`.
#[derive(Debug)]
struct TestDroppedFile(PathBuf);
impl egui::DroppedFile for TestDroppedFile {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("not needed in this test".into())
    }
}

/// Drag & drop from the file manager: a file dropped on the window
/// (`i.raw.dropped_files`) is imported into the media pool exactly as
/// from the file dialog.
#[test]
fn dropping_a_file_from_the_file_manager_imports_it_into_the_pool() {
    let path = make_wav("dropped.wav");
    let mut app = VenturiApp::default();

    let ctx = egui::Context::default();
    let mut input = egui::RawInput::default();
    input.dropped_files = vec![std::sync::Arc::new(TestDroppedFile(path.clone()))];
    let mut output = ctx.run_ui(input, |ui| app.poll_dropped_files(ui.ctx()));
    output.textures_delta.clear();
    app.wait_for_import();

    assert!(app.import_warnings.is_empty(), "{:?}", app.import_warnings);
    // +1: the project timeline, created on the fly by the import (see
    // `ensure_timeline_for`), shows up in the pool too.
    assert_eq!(
        app.session
            .project
            .media_pool
            .values()
            .filter(|m| m.compound.is_none())
            .count(),
        1
    );
    let item = app
        .session
        .project
        .media_pool
        .values()
        .find(|m| m.compound.is_none())
        .unwrap();
    assert_eq!(item.path, path);
}

/// With the "use proxy" toggle off nothing is generated; turning it back on
/// (or changing quality) the media already in the pool go back into the queue.
#[test]
fn disabling_proxies_stops_generation_and_enabling_requeues_the_pool() {
    let dir = std::env::temp_dir().join("vv-app-proxy-toggle-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clip.mp4");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );

    let mut app = VenturiApp::default();
    app.settings.proxy_enabled = false;
    app.import_media_files(vec![path]);
    app.wait_for_import();
    assert!(
        app.proxy_worker.is_none(),
        "with the toggle off no proxy starts"
    );

    app.settings.proxy_enabled = true;
    app.apply_proxy_settings();
    let worker = app.proxy_worker.as_ref().unwrap();
    assert_eq!(worker.progress().total, 1);

    app.settings.proxy_quality = vv_media::proxy::ProxyQuality::Low;
    app.apply_proxy_settings();
    let worker = app.proxy_worker.as_ref().unwrap();
    assert_eq!(
        worker.quality(),
        vv_media::proxy::ProxyQuality::Low,
        "changing quality restarts the queue"
    );
    assert_eq!(worker.progress().total, 1);

    app.settings.proxy_enabled = false;
    app.apply_proxy_settings();
    assert!(
        app.proxy_worker.is_none(),
        "turning the toggle off must drop the queue"
    );
}

/// The export pauses the proxy generation (which would otherwise
/// contend for CPU and ffmpeg with it, holding it up) and resumes it at the
/// end — but it does not resume a pause chosen by the user.
#[test]
fn export_pauses_the_proxy_queue_and_resumes_it_afterwards() {
    let mut app = VenturiApp::default();
    app.proxy_worker = Some(proxy_worker::ProxyWorker::spawn(Default::default()));

    app.pause_proxies_for_export();
    assert!(app.proxy_worker.as_ref().unwrap().is_paused());
    app.resume_proxies_after_export();
    assert!(!app.proxy_worker.as_ref().unwrap().is_paused());

    app.proxy_worker.as_ref().unwrap().set_paused(true);
    app.pause_proxies_for_export();
    app.resume_proxies_after_export();
    assert!(
        app.proxy_worker.as_ref().unwrap().is_paused(),
        "a user pause must not be undone by the end of the export"
    );
}

fn browse_fixture(name: &str) -> (VenturiApp, MediaId) {
    let dir = std::env::temp_dir().join("vv-app-browse-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=25:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ],
        &path,
    );
    let mut app = VenturiApp::default();
    let media_id = app.add_media_to_pool(path).unwrap();
    app.preview_media(media_id);
    app.browsing_media = Some(media_id);
    (app, media_id)
}

#[test]
fn space_plays_the_media_pool_preview_instead_of_leaving_it() {
    let (mut app, media_id) = browse_fixture("space.mp4");
    app.toggle_playback();
    assert_eq!(app.browsing_media, Some(media_id));
    assert!(app.is_timeline_playing());

    std::thread::sleep(std::time::Duration::from_millis(200));
    app.drive_browse_playback();
    assert!(
        app.browse_playhead > 0,
        "the preview playhead should have advanced"
    );

    app.toggle_playback();
    assert!(!app.is_timeline_playing());
}

#[test]
fn preview_plays_the_media_audio_and_leaving_it_restores_the_timeline_mix() {
    let (mut app, _) = browse_fixture("audio.mp4");
    let fps = app.browse_fps();
    app.timeline_audio();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        app.sync_timeline_audio();
        if !app.timeline_audio().has_pending_buffers() {
            app.sync_timeline_audio();
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "audio decode never arrived"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let peak = |app: &VenturiApp| {
        app.timeline_audio
            .as_ref()
            .unwrap()
            .render(10, fps, 5)
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()))
    };
    assert!(peak(&app) > 0.1, "the preview must play");

    app.stop_browsing();
    app.sync_timeline_audio();
    assert_eq!(
        peak(&app),
        0.0,
        "outside the preview the timeline mix is back, empty here"
    );
}

#[test]
fn in_out_marks_on_the_preview_trim_the_clip_dropped_on_the_timeline() {
    let (mut app, media_id) = browse_fixture("marks.mp4");
    app.seek_browse(10);
    app.mark_at_playhead(true);
    app.seek_browse(29);
    app.mark_at_playhead(false);
    let (source_in, source_out) = app.browse_marks.resolve(app.browse_total_frames());
    assert_eq!((source_in, source_out), (10, 30));

    app.add_media_to_timeline_at(
        timeline_ui::MediaDrag {
            media_id,
            source_in,
            source_out,
            streams: timeline_ui::DragStreams::All,
        },
        5,
        timeline_ui::MediaDropTarget::Default,
    );
    let timeline = &app.session.project.timelines[app.timeline_id.unwrap()];
    let clips: Vec<_> = timeline.tracks.iter().flat_map(|t| &t.clips).collect();
    assert_eq!(clips.len(), 2, "video + audio");
    for clip in clips {
        assert_eq!(
            (clip.source_in(), clip.source_out(), clip.timeline_start),
            (10, 30, 5)
        );
    }
}

#[test]
fn video_only_and_audio_only_drags_insert_just_that_stream() {
    for (streams, kind) in [
        (timeline_ui::DragStreams::VideoOnly, TrackKind::Video),
        (timeline_ui::DragStreams::AudioOnly, TrackKind::Audio),
    ] {
        let (mut app, media_id) = browse_fixture("streams.mp4");
        let meta = app.session.project.media_pool[media_id].meta.clone();
        app.add_media_to_timeline_at(
            timeline_ui::MediaDrag {
                streams,
                ..timeline_ui::MediaDrag::whole(media_id, &meta)
            },
            0,
            timeline_ui::MediaDropTarget::Default,
        );
        let timeline = &app.session.project.timelines[app.timeline_id.unwrap()];
        let kinds: Vec<TrackKind> = timeline
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(|_| t.kind))
            .collect();
        assert_eq!(kinds, vec![kind], "{streams:?}");
    }
}

#[test]
fn in_out_keys_on_the_timeline_set_the_export_range() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 100);
    app.timeline_state.playhead = 20;
    app.mark_at_playhead(true);
    app.timeline_state.playhead = 59;
    app.mark_at_playhead(false);
    assert_eq!(app.timeline_state.export_marks.resolve(100), (20, 60));
}

/// End-to-end test of the reported bug ("the buffer always stops at the
/// edge of the next clip"): unlike the old per-clip
/// system, `render_ahead` must buffer *past* the end of the
/// active clip, inside the next clip, BEFORE the playhead
/// reaches it — a hard cut between two different media, no special
/// case needed.
#[test]
fn buffered_timeline_ranges_covers_the_next_clip_before_the_playhead_reaches_it() {
    let dir = std::env::temp_dir().join("vv-app-buffered-ranges-cut-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path_a = dir.join("clip_a.mp4");
    let path_b = dir.join("clip_b.mp4");
    for (path, duration) in [(&path_a, 2), (&path_b, 1)] {
        vv_media::test_support::ffmpeg(
            &[
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc=size=320x240:rate=25:duration={duration}"),
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ],
            path,
        );
    }

    let mut app = VenturiApp::default();
    app.import_media(path_a);
    app.import_media(path_b);
    let mut media_ids = app
        .session
        .project
        .media_pool
        .iter()
        .filter(|(_, item)| item.compound.is_none())
        .map(|(id, _)| id);
    let media_a = media_ids.next().unwrap();
    let media_b = media_ids.next().unwrap();
    drop(media_ids);

    let whole = |app: &VenturiApp, id| {
        timeline_ui::MediaDrag::whole(id, &app.session.project.media_pool[id].meta)
    };
    app.add_media_to_timeline_at(
        whole(&app, media_a),
        0,
        timeline_ui::MediaDropTarget::Default,
    ); // [0,50)
    app.add_media_to_timeline_at(
        whole(&app, media_b),
        50,
        timeline_ui::MediaDropTarget::Default,
    ); // [50,75), adjacent
    let timeline_id = app.timeline_id.unwrap();
    let clip_b = app.session.project.timelines[timeline_id].tracks[0].clips[1].clone();

    // Playhead near the end of the first clip: the lookahead
    // window of `render_ahead` (3s) amply crosses the
    // cut at 50.
    app.timeline_state.playhead = 45;
    app.sync_render_ahead();

    let start = std::time::Instant::now();
    loop {
        let covers_next_clip = app
            .buffered_timeline_ranges()
            .iter()
            .any(|&(s, e)| s < clip_b.timeline_end() && e >= clip_b.timeline_start);
        if covers_next_clip {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "timeout: the next clip was never buffered ahead"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn save_project_to_then_load_project_from_round_trips_and_resets_ui_state() {
    let mut app = VenturiApp::default();
    let clip_id = make_timeline_with_clip(&mut app, 0, 0, 10);
    let timeline_id = app.timeline_id.unwrap();
    app.timeline_state.selected = BTreeSet::from([(0, clip_id)]);
    app.timeline_state.playhead = 5;

    let dir = std::env::temp_dir().join("vv-app-persistence-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("project.vvproj");

    app.save_project_to(&path);
    assert!(app.project_error.is_none(), "{:?}", app.project_error);
    assert_eq!(app.session.path(), Some(path.as_path()));

    // A "new" project in memory (another clip, another
    // selection/playhead): loading must replace everything, not
    // merge.
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 999);
    app.timeline_state.playhead = 42;

    app.load_project_from(path.clone());
    assert!(app.project_error.is_none(), "{:?}", app.project_error);
    assert_eq!(app.session.path(), Some(path.as_path()));

    let loaded_timeline_id = app.timeline_id.expect("timeline expected after load");
    assert_eq!(
        loaded_timeline_id, timeline_id,
        "same TimelineId as before: the ids round-trip"
    );
    assert_eq!(
        app.session.project.timelines[loaded_timeline_id].tracks[0].clips[0].id,
        clip_id
    );
    // UI state of the previous project cleared, not inherited from the
    // old `app` nor left over from the project just overwritten.
    assert!(app.timeline_state.selected.is_empty());
    assert_eq!(app.timeline_state.playhead, 0);
}

#[test]
fn load_project_from_a_bad_path_sets_project_error_without_touching_the_current_project() {
    let mut app = VenturiApp::default();
    let clip_id = make_timeline_with_clip(&mut app, 0, 0, 10);
    let timeline_id = app.timeline_id.unwrap();

    app.load_project_from(std::env::temp_dir().join("vv-app-persistence-test/nope.vvproj"));

    assert!(app.project_error.is_some());
    // The current project (never saved) stays intact: a failed load
    // must not erase unsaved work.
    assert_eq!(app.timeline_id, Some(timeline_id));
    assert_eq!(
        app.session.project.timelines[timeline_id].tracks[0].clips[0].id,
        clip_id
    );
}

#[test]
fn export_otio_to_writes_the_file_and_reports_failures() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);
    let timeline_id = app.timeline_id.unwrap();
    let dir = std::env::temp_dir().join("vv-app-otio-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("timeline.otio");

    app.export_otio_to(timeline_id, &path);
    assert!(app.project_error.is_none());
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("\"Timeline.1\"")
    );

    app.export_otio_to(timeline_id, &dir.join("nope/timeline.otio"));
    assert!(app.project_error.is_some());
}

#[test]
fn import_otio_from_adds_a_timeline_and_reports_skipped_clips() {
    let dir = std::env::temp_dir().join("vv-app-otio-import-test");
    std::fs::create_dir_all(&dir).unwrap();
    let media = dir.join("clip.mp4");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &media,
    );
    let range = |start: f64, duration: f64| {
        serde_json::json!({
            "OTIO_SCHEMA": "TimeRange.1",
            "start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": start, "rate": 25.0 },
            "duration": { "OTIO_SCHEMA": "RationalTime.1", "value": duration, "rate": 25.0 },
        })
    };
    let clip = |url: &str| {
        serde_json::json!({
            "OTIO_SCHEMA": "Clip.1",
            "name": url,
            "source_range": range(5.0, 20.0),
            "media_reference": { "OTIO_SCHEMA": "ExternalReference.1", "target_url": url },
        })
    };
    let otio = serde_json::json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": "Importata",
        "tracks": { "OTIO_SCHEMA": "Stack.1", "children": [{
            "OTIO_SCHEMA": "Track.1",
            "kind": "Video",
            "children": [clip("clip.mp4"), clip("gone.mp4")],
        }]},
    });
    let otio_path = dir.join("timeline.otio");
    std::fs::write(&otio_path, otio.to_string()).unwrap();

    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);
    let old_timeline = app.timeline_id.unwrap();
    app.session.set_path(Some(dir.join("old.vvproj")));
    app.import_otio_from(&otio_path);
    app.wait_for_otio_import();

    assert!(app.project_error.is_none(), "{:?}", app.project_error);
    assert!(
        app.session.otio_awaiting_decision().is_none(),
        "no media with the same name"
    );
    assert_eq!(app.session.path(), Some(dir.join("old.vvproj").as_path()));
    assert!(app.has_unsaved_changes());
    assert!(app.session.project.timelines.contains_key(old_timeline));
    let timeline_id = app.timeline_id.unwrap();
    assert_ne!(timeline_id, old_timeline, "the imported one is opened");
    let timeline = &app.session.project.timelines[timeline_id];
    assert_eq!(timeline.name, "timeline", "named after the file");
    let timeline_item = app
        .session
        .project
        .media_pool
        .values()
        .find(|m| m.compound == Some(timeline_id))
        .expect("the timeline shows up in the pool");
    assert_eq!(timeline_item.folder, None);
    let (folder_id, folder) = app.session.project.folders.iter().next().unwrap();
    assert_eq!(folder.name, "timeline");
    let otio_media: Vec<_> = app
        .session
        .project
        .media_pool
        .values()
        .filter(|m| m.path.starts_with(&dir))
        .collect();
    assert_eq!(otio_media.len(), 2);
    assert!(otio_media.iter().all(|m| m.folder == Some(folder_id)));
    let clips = &timeline.tracks[0].clips;
    assert_eq!(clips.len(), 2, "the missing one is kept offline");
    let clip = &clips[0];
    assert_eq!(
        (clip.timeline_start, clip.timeline_len, clip.source_in()),
        (0, 20, 5)
    );
    let warnings = &app.import_warnings;
    assert!(
        warnings.iter().any(|w| w.contains("gone.mp4")),
        "{warnings:?}"
    );

    // Relinking probes the file: the offline meta did not know its size.
    let found_dir = dir.join("found");
    std::fs::create_dir_all(&found_dir).unwrap();
    std::fs::copy(&media, found_dir.join("gone.mp4")).unwrap();
    let offline = app
        .session
        .project
        .media_pool
        .iter()
        .find(|(_, m)| m.path.ends_with("gone.mp4"))
        .map(|(id, _)| id)
        .unwrap();
    assert_eq!(app.session.project.media_pool[offline].meta.width, 0);
    app.relink_media(&found_dir, &[offline]);
    app.wait_for_relink();
    let meta = &app.session.project.media_pool[offline].meta;
    assert_eq!((meta.width, meta.height), (320, 240));
}

/// Importing twice: the second time the media names match and the user
/// chooses between reusing them and a new folder.
#[test]
fn otio_media_already_in_the_pool_wait_for_the_reuse_choice() {
    let dir = std::env::temp_dir().join("vv-app-otio-reuse-test");
    std::fs::create_dir_all(&dir).unwrap();
    let otio = serde_json::json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": "Edit",
        "tracks": { "OTIO_SCHEMA": "Stack.1", "children": [{
            "OTIO_SCHEMA": "Track.1",
            "kind": "Video",
            "children": [{
                "OTIO_SCHEMA": "Clip.1",
                "name": "a",
                "source_range": {
                    "OTIO_SCHEMA": "TimeRange.1",
                    "start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 25.0 },
                    "duration": { "OTIO_SCHEMA": "RationalTime.1", "value": 10.0, "rate": 25.0 },
                },
                "media_reference": { "OTIO_SCHEMA": "ExternalReference.1", "target_url": "a.mp4" },
            }],
        }]},
    });
    let otio_path = dir.join("edit.otio");
    std::fs::write(&otio_path, otio.to_string()).unwrap();
    let mut app = VenturiApp::default();
    app.import_otio_from(&otio_path);
    app.wait_for_otio_import();
    let first_media = app
        .session
        .project
        .media_pool
        .iter()
        .find(|(_, m)| m.compound.is_none())
        .map(|(id, _)| id)
        .unwrap();
    let pool_len = app.session.project.media_pool.len();

    app.import_otio_from(&otio_path);
    app.wait_for_otio_import();
    let (_, matches) = app
        .session
        .otio_awaiting_decision()
        .expect("a.mp4 is already there");
    assert_eq!(matches, 1);
    app.finish_otio_import(Some(true));

    assert_eq!(
        app.session.project.media_pool.len(),
        pool_len + 1,
        "only the timeline item"
    );
    assert_eq!(
        app.session.project.folders.len(),
        1,
        "no folder for nothing"
    );
    let clip = &app.session.project.timelines[app.timeline_id.unwrap()].tracks[0].clips[0];
    assert!(matches!(clip.source, vv_core::ClipSource::Media(m) if m == first_media));

    app.import_otio_from(&otio_path);
    app.wait_for_otio_import();
    assert!(app.session.otio_awaiting_decision().is_some());
    app.finish_otio_import(Some(false));
    assert_eq!(app.session.project.media_pool.len(), pool_len + 3);
    assert_eq!(app.session.project.folders.len(), 2);
}

#[test]
fn unsaved_changes_follow_edits_and_saves() {
    let mut app = VenturiApp::default();
    assert!(!app.has_unsaved_changes(), "empty project");
    make_timeline_with_clip(&mut app, 0, 0, 10);
    assert!(app.has_unsaved_changes());

    let dir = std::env::temp_dir().join("vv-app-unsaved-test");
    std::fs::create_dir_all(&dir).unwrap();
    app.save_project_to(&dir.join("p.vvproj"));
    assert!(!app.has_unsaved_changes());

    app.create_timeline("Other".into(), vv_core::Rational::new(25, 1), (64, 48));
    assert!(app.has_unsaved_changes(), "timeline created after saving");
}

/// With unsaved changes, opening waits for the answer; "Cancel"
/// and a failed save leave the project as it is.
#[test]
fn switching_project_with_unsaved_changes_waits_and_keeps_the_project_on_failure() {
    let mut app = VenturiApp::default();
    let clip_id = make_timeline_with_clip(&mut app, 0, 0, 10);
    let timeline_id = app.timeline_id.unwrap();

    app.request_project_switch(ProjectSwitch::Open);
    assert_eq!(app.pending_project_switch, Some(ProjectSwitch::Open));
    app.resolve_unsaved_changes(UnsavedChoice::Cancel);
    assert_eq!(app.pending_project_switch, None);

    app.session
        .set_path(Some(std::env::temp_dir().join("vv-app-nope/dir/p.vvproj")));
    app.request_project_switch(ProjectSwitch::Open);
    app.resolve_unsaved_changes(UnsavedChoice::Save);
    assert!(app.project_error.is_some(), "save failed");
    assert!(app.has_unsaved_changes());
    assert_eq!(
        app.session.project.timelines[timeline_id].tracks[0].clips[0].id,
        clip_id
    );
}

/// "New project" asks about unsaved changes, then starts from an empty,
/// untitled project.
#[test]
fn new_project_discards_after_confirmation() {
    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);
    app.session
        .set_path(Some(std::env::temp_dir().join("vv-app-new-project.vvproj")));

    app.request_project_switch(ProjectSwitch::New);
    assert_eq!(app.pending_project_switch, Some(ProjectSwitch::New));
    app.resolve_unsaved_changes(UnsavedChoice::Discard);

    assert!(app.session.project.timelines.is_empty());
    assert!(app.session.project.media_pool.is_empty());
    assert_eq!(app.timeline_id, None);
    assert_eq!(app.session.path(), None);
    assert!(!app.has_unsaved_changes());
}

/// The waveform cache may be missing (deleted, another machine):
/// opening the project regenerates it.
#[test]
fn opening_a_project_regenerates_missing_waveforms() {
    let dir = std::env::temp_dir().join("vv-app-waveform-on-load-test");
    std::fs::create_dir_all(&dir).unwrap();
    let media = dir.join("tone.mp4");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=25:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ],
        &media,
    );

    // Hash never seen: no waveform cached for this media.
    let content_hash = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let mut project = vv_core::Project::default();
    project.media_pool.insert(vv_core::MediaItem {
        path: media.clone(),
        meta: vv_media::probe(&media).unwrap(),
        content_hash,
        compound: None,
        folder: None,
    });
    let project_path = dir.join("p.vvproj");
    vv_core::save_project(&project, &project_path).unwrap();
    assert!(!vv_media::waveform::waveform_exists(content_hash, 0));

    let mut app = VenturiApp::default();
    app.load_project_from(project_path);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !vv_media::waveform::waveform_exists(content_hash, 0) {
        assert!(
            std::time::Instant::now() < deadline,
            "waveform not generated"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = std::fs::remove_file(vv_media::waveform::waveform_path_for(content_hash, 0));
}

fn make_wav(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vv-app-audio-only-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
        ],
        &path,
    );
    path
}

/// An audio-only media enters the pool without a proxy or thumbnail, and on the
/// timeline it becomes only an audio clip (creating the track if missing).
#[test]
fn an_audio_only_file_imports_and_drops_as_an_audio_clip() {
    let path = make_wav("tone.wav");
    let mut app = VenturiApp::default();
    app.import_media_files(vec![path]);
    app.wait_for_import();
    assert!(app.import_warnings.is_empty(), "{:?}", app.import_warnings);
    let (media_id, item) = app
        .session
        .project
        .media_pool
        .iter()
        .find(|(_, item)| item.compound.is_none())
        .unwrap();
    assert!(!item.meta.has_video);
    assert!(
        app.proxy_worker.is_none(),
        "no proxy for an audio-only media"
    );
    assert!(app.thumbnails.is_empty());

    let timeline_id = app.timeline_id.unwrap();
    let timeline = &app.session.project.timelines[timeline_id];
    assert_eq!(timeline.resolution, (1920, 1080), "default timeline");
    assert!(
        timeline.tracks.iter().all(|t| t.kind == TrackKind::Audio),
        "an audio-only import must not create any video track, not even an empty one"
    );
    {
        let timeline = &mut app.session.project.timelines[timeline_id];
        timeline.tracks.clear();
    }

    let meta = item.meta.clone();
    app.add_media_to_timeline_at(
        timeline_ui::MediaDrag::whole(media_id, &meta),
        10,
        timeline_ui::MediaDropTarget::Default,
    );
    let timeline = &app.session.project.timelines[timeline_id];
    assert!(
        timeline.tracks.iter().all(|t| t.kind == TrackKind::Audio),
        "the drop must not have created any video track"
    );
    let (_, audio) = timeline
        .tracks_of_kind(TrackKind::Audio)
        .next()
        .expect("track created");
    assert_eq!(audio.clips.len(), 1);
    let clip = &audio.clips[0];
    assert_eq!(clip.timeline_start, 10);
    assert_eq!(clip.linked_group, None);
    assert_eq!(clip.timeline_len, 50, "2 s a 25 fps");
}

fn make_png(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vv-app-image-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:size=640x360:rate=1:duration=1",
            "-frames:v",
            "1",
            "-update",
            "1",
        ],
        &path,
    );
    path
}

/// An image enters the pool as a video media without audio, without
/// a proxy, with `duration_frames` the sentinel of
/// `vv_core::IMAGE_DURATION_FRAMES` — and dragged "whole" onto the
/// timeline it produces a 5s clip by default (shortenable/
/// lengthenable like any clip), not one as long as the
/// sentinel.
#[test]
fn an_image_file_imports_and_drops_as_a_five_second_clip_without_audio() {
    let path = make_png("still.png");
    let mut app = VenturiApp::default();
    app.import_media_files(vec![path.clone()]);
    app.wait_for_import();
    assert!(app.import_warnings.is_empty(), "{:?}", app.import_warnings);

    let (media_id, item) = app
        .session
        .project
        .media_pool
        .iter()
        .find(|(_, item)| item.compound.is_none())
        .unwrap();
    assert!(item.meta.is_image());
    assert!(item.meta.has_video);
    assert!(!item.meta.has_audio);
    assert_eq!((item.meta.width, item.meta.height), (640, 360));
    assert_eq!(item.meta.duration_frames, vv_core::IMAGE_DURATION_FRAMES);
    assert!(app.proxy_worker.is_none(), "no proxy for an image");

    let timeline_id = app
        .timeline_id
        .expect("an image creates the timeline like a video");
    let meta = item.meta.clone();
    app.add_media_to_timeline_at(
        timeline_ui::MediaDrag::whole(media_id, &meta),
        0,
        timeline_ui::MediaDropTarget::Default,
    );
    let timeline = &app.session.project.timelines[timeline_id];
    let (_, video) = timeline
        .tracks_of_kind(TrackKind::Video)
        .next()
        .expect("track created");
    assert_eq!(video.clips.len(), 1);
    let clip = &video.clips[0];
    assert_eq!(clip.timeline_len, 5 * 25, "5 s by default at 25 fps");
    assert!(
        timeline
            .tracks
            .iter()
            .all(|t| t.kind == TrackKind::Video || t.clips.is_empty()),
        "an image has no audio: no audio clip must appear"
    );
}

/// Regression: dragging an audio-only media onto the timeline must
/// never create (nor reuse from empty) a video track — not even
/// the first time, when it is also the one giving birth to the timeline.
#[test]
fn dropping_audio_only_media_creates_no_video_track() {
    let mut app = VenturiApp::default();
    let media_id = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/tmp/vv-audio-only.wav".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 50,
            fps: vv_media::AUDIO_ONLY_FPS,
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 48000,
            channels: 2,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let meta = app.session.project.media_pool[media_id].meta.clone();

    app.add_media_to_timeline_at(
        timeline_ui::MediaDrag::whole(media_id, &meta),
        0,
        timeline_ui::MediaDropTarget::Default,
    );

    let timeline_id = app
        .timeline_id
        .expect("the drop creates the timeline on the fly");
    let timeline = &app.session.project.timelines[timeline_id];
    assert!(
        timeline.tracks.iter().all(|t| t.kind == TrackKind::Audio),
        "no video track for an audio-only drop on an empty project: {:?}",
        timeline.tracks.iter().map(|t| t.kind).collect::<Vec<_>>()
    );
}

fn close_request_commands(app: &mut VenturiApp) -> Vec<egui::ViewportCommand> {
    let ctx = egui::Context::default();
    let mut input = egui::RawInput::default();
    input
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    let mut output = ctx.run_ui(input, |ui| app.handle_close_request(ui.ctx()));
    output.textures_delta.clear();
    output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

/// Closing the window with unsaved changes stops on the dialog;
/// without changes, or after "Don't save", it exits.
#[test]
fn closing_the_window_asks_to_save_unsaved_changes() {
    let mut app = VenturiApp::default();
    let commands = close_request_commands(&mut app);
    assert!(
        !commands.contains(&egui::ViewportCommand::CancelClose),
        "nothing to save"
    );

    let mut app = VenturiApp::default();
    make_timeline_with_clip(&mut app, 0, 0, 10);
    let commands = close_request_commands(&mut app);
    assert!(commands.contains(&egui::ViewportCommand::CancelClose));
    assert_eq!(app.pending_project_switch, Some(ProjectSwitch::Quit));

    app.resolve_unsaved_changes(UnsavedChoice::Cancel);
    assert!(!app.quit_confirmed);

    close_request_commands(&mut app);
    app.resolve_unsaved_changes(UnsavedChoice::Discard);
    assert!(app.quit_confirmed);
    let commands = close_request_commands(&mut app);
    assert!(commands.contains(&egui::ViewportCommand::Close));
    assert!(!commands.contains(&egui::ViewportCommand::CancelClose));
}

/// A media with *two* audio streams (e.g. stereo mix + separate 5.1, the
/// real bug that motivated `Clip::audio_stream_index`): the import must
/// create one audio clip per stream, on separate audio tracks (the
/// second created on the fly, since by default the timeline has a single
/// one), and link them all together (video included) in the same
/// group — see the docs of `insert_media_clip`.
#[test]
fn add_media_to_timeline_creates_one_audio_clip_per_audio_stream() {
    let dir = std::env::temp_dir().join("vv-app-multi-audio-import-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("two_audio_streams.mp4");

    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x48:rate=25:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=48000:duration=1",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-map",
            "2:a",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ],
        &path,
    );

    let mut app = VenturiApp::default();
    app.import_media(path.clone());
    let media_id = app
        .session
        .project
        .media_pool
        .iter()
        .find(|(_, item)| item.compound.is_none())
        .unwrap()
        .0;
    app.add_media_to_timeline(media_id);

    let timeline_id = app.timeline_id.unwrap();
    let tl = &app.session.project.timelines[timeline_id];

    assert_eq!(
        tl.tracks.len(),
        3,
        "video + 2 audio tracks (one created on the fly for the second stream)"
    );
    assert_eq!(tl.tracks[0].kind, TrackKind::Video);
    assert_eq!(tl.tracks[1].kind, TrackKind::Audio);
    assert_eq!(tl.tracks[2].kind, TrackKind::Audio);
    assert_eq!(tl.tracks[0].clips.len(), 1);
    assert_eq!(tl.tracks[1].clips.len(), 1);
    assert_eq!(tl.tracks[2].clips.len(), 1);

    let video_clip = &tl.tracks[0].clips[0];
    let audio_clip_0 = &tl.tracks[1].clips[0];
    let audio_clip_1 = &tl.tracks[2].clips[0];

    assert_eq!(audio_clip_0.audio_stream_index, 0);
    assert_eq!(audio_clip_1.audio_stream_index, 1);

    // Video and *all* the audio streams end up in the same linked
    // group (`Clip::linked_group`), not only the first.
    let group = video_clip.linked_group.expect("the video is linked");
    assert_eq!(audio_clip_0.linked_group, Some(group));
    assert_eq!(
        audio_clip_1.linked_group,
        Some(group),
        "the second audio stream is part of the same group too"
    );
}

#[test]
fn undoing_an_import_removes_its_media_and_the_timeline_it_created() {
    let paths = vec![make_wav("undo-a.wav"), make_wav("undo-b.wav")];
    let mut app = VenturiApp::default();
    app.import_media_files(paths);
    app.wait_for_import();
    let media: Vec<MediaId> = app.session.project.media_pool.keys().collect();
    let timeline_id = app.timeline_id.expect("created by the import");
    assert_eq!(app.session.history.position(), 1);

    app.undo();
    assert!(app.session.project.media_pool.is_empty());
    assert!(app.session.project.timelines.is_empty());
    assert_eq!(app.timeline_id, None);
    assert!(app.media_pool_state.selected.is_empty());
    assert_eq!(app.browsing_media, None);

    app.redo();
    assert_eq!(
        app.session.project.media_pool.keys().collect::<Vec<_>>(),
        media
    );
    assert!(app.session.project.timelines.contains_key(timeline_id));
}

#[test]
fn a_single_file_import_and_its_timeline_are_one_undo_step() {
    let mut app = VenturiApp::default();
    app.import_media(make_wav("undo-single.wav"));
    assert!(app.timeline_id.is_some());

    assert_eq!(
        app.session.history.labels().collect::<Vec<_>>(),
        vec![vv_core::CommandLabel::ImportMedia]
    );
    app.undo();
    assert!(app.session.project.timelines.is_empty());
}

#[test]
fn a_replaced_project_does_not_inherit_the_last_export_destination() {
    let mut app = VenturiApp::default();
    app.last_export_settings = Some(export::ExportSettings::new("/old/project.mp4".into()));
    app.new_project();
    assert!(app.last_export_settings.is_none());
}
