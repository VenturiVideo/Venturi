use super::*;

/// Timeline at 25 fps with the given tracks and a 25 fps media of 200 frames.
fn setup(kinds: &[TrackKind]) -> (Project, TimelineId, MediaId) {
    let mut project = Project::default();
    let timeline = project.timelines.insert(Timeline {
        name: "Timeline 1".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: kinds.iter().map(|k| Track::new(*k)).collect(),
        markers: Vec::new(),
        master: Default::default(),
    });
    let media = project.media_pool.insert(MediaItem {
        path: "a.mp4".into(),
        meta: MediaMeta {
            duration_frames: 200,
            fps: Rational::new(25, 1),
            width: 1920,
            height: 1080,
            has_video: true,
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
    (project, timeline, media)
}

/// Media clip showing source `source_in..source_out` from `start`.
fn add(
    project: &mut Project,
    timeline: TimelineId,
    track: usize,
    media: MediaId,
    (source_in, source_out): (FrameIdx, FrameIdx),
    start: FrameIdx,
) -> ClipId {
    let id = project.alloc_clip_id();
    let clip = Clip::from_source_range(
        id,
        ClipSource::Media(media),
        source_in,
        source_out,
        start,
        Rational::one(),
    );
    project.timelines[timeline].tracks[track].insert_sorted(clip);
    id
}

fn span(project: &Project, timeline: TimelineId, track: usize, id: ClipId) -> (FrameIdx, FrameIdx) {
    let clip = project.timelines[timeline].clip(track, id).unwrap();
    (clip.timeline_start, clip.timeline_len)
}

#[test]
fn doubling_the_speed_halves_the_clip_and_ripples_every_track() {
    let (mut project, tl, media) = setup(&[TrackKind::Video, TrackKind::Audio]);
    let a = add(&mut project, tl, 0, media, (20, 120), 0);
    let b = add(&mut project, tl, 0, media, (0, 50), 100);
    let c = add(&mut project, tl, 1, media, (0, 30), 120);
    let mut history = History::default();
    history.do_command(
        &mut project,
        Box::new(SetClipSpeed::new(
            tl,
            vec![(0, a)],
            Rational::new(2, 1),
            false,
            SpeedFit::Ripple,
        )),
    );

    let clip = project.timelines[tl].clip(0, a).unwrap();
    assert_eq!((clip.timeline_start, clip.timeline_len), (0, 50));
    assert_eq!(clip.source_in(), 20, "the source range stays");
    assert_eq!(clip.source_frame_at(49), 118);
    assert_eq!(span(&project, tl, 0, b), (50, 50));
    assert_eq!(span(&project, tl, 1, c), (70, 30), "A/V sync kept");

    history.undo(&mut project);
    assert_eq!(span(&project, tl, 0, a), (0, 100));
    assert_eq!(span(&project, tl, 0, b), (100, 50));
    assert_eq!(
        project.timelines[tl].clip(0, a).unwrap().speed(),
        Rational::one()
    );
}

#[test]
fn slowing_down_lengthens_the_clip_and_pushes_what_follows() {
    let (mut project, tl, media) = setup(&[TrackKind::Video]);
    let a = add(&mut project, tl, 0, media, (0, 40), 10);
    let b = add(&mut project, tl, 0, media, (0, 10), 50);
    let mut history = History::default();
    history.do_command(
        &mut project,
        Box::new(SetClipSpeed::new(
            tl,
            vec![(0, a)],
            Rational::from_percent(50.0),
            false,
            SpeedFit::Ripple,
        )),
    );
    assert_eq!(span(&project, tl, 0, a), (10, 80));
    assert_eq!(span(&project, tl, 0, b), (90, 10));
    let clip = project.timelines[tl].clip(0, a).unwrap();
    assert_eq!(
        (0..4)
            .map(|t| clip.source_frame_at(10 + t))
            .collect::<Vec<_>>(),
        [0, 0, 1, 1],
        "every source frame held twice"
    );
}

/// A clip on another track straddling the old end stays put: the ripple
/// backwards stops where the clip after it would overlap it.
#[test]
fn the_backward_ripple_never_overlaps_a_clip_that_stays() {
    let (mut project, tl, media) = setup(&[TrackKind::Video, TrackKind::Audio]);
    let a = add(&mut project, tl, 0, media, (0, 100), 0);
    let b = add(&mut project, tl, 0, media, (0, 20), 100);
    let _straddling = add(&mut project, tl, 1, media, (0, 100), 50);
    let d = add(&mut project, tl, 1, media, (0, 20), 160);
    let mut history = History::default();
    history.do_command(
        &mut project,
        Box::new(SetClipSpeed::new(
            tl,
            vec![(0, a)],
            Rational::new(4, 1),
            false,
            SpeedFit::Ripple,
        )),
    );
    assert_eq!(span(&project, tl, 0, a), (0, 25));
    assert_eq!(
        span(&project, tl, 1, d),
        (150, 20),
        "stops at the straddling clip's end"
    );
    assert_eq!(span(&project, tl, 0, b), (90, 20));
}

#[test]
fn without_ripple_the_clip_keeps_its_length_within_the_media() {
    let (mut project, tl, media) = setup(&[TrackKind::Video]);
    let a = add(&mut project, tl, 0, media, (100, 160), 0);
    let b = add(&mut project, tl, 0, media, (0, 10), 60);
    let mut history = History::default();
    history.do_command(
        &mut project,
        Box::new(SetClipSpeed::new(
            tl,
            vec![(0, a)],
            Rational::new(2, 1),
            true,
            SpeedFit::KeepLength,
        )),
    );
    let clip = project.timelines[tl].clip(0, a).unwrap();
    assert_eq!(
        (clip.timeline_len, clip.source_in(), clip.source_out()),
        (50, 100, 199),
        "the media ends after 100 more source frames, shown every other one"
    );
    assert!(clip.pitch_correction);
    assert_eq!(span(&project, tl, 0, b), (60, 10));
}

/// Linked video and audio share their end: the ripple happens once.
#[test]
fn a_linked_group_ripples_once() {
    let (mut project, tl, media) = setup(&[TrackKind::Video, TrackKind::Audio]);
    let v = add(&mut project, tl, 0, media, (0, 100), 0);
    let a = add(&mut project, tl, 1, media, (0, 100), 0);
    let after = add(&mut project, tl, 1, media, (0, 10), 100);
    let mut history = History::default();
    history.do_command(
        &mut project,
        Box::new(SetClipSpeed::new(
            tl,
            vec![(0, v), (1, a)],
            Rational::new(2, 1),
            false,
            SpeedFit::Ripple,
        )),
    );
    assert_eq!(span(&project, tl, 0, v), (0, 50));
    assert_eq!(span(&project, tl, 1, a), (0, 50));
    assert_eq!(span(&project, tl, 1, after), (50, 10));
}

#[test]
fn media_seconds_follow_the_speed() {
    let (mut project, tl, media) = setup(&[TrackKind::Audio]);
    let a = add(&mut project, tl, 0, media, (50, 150), 0);
    let mut history = History::default();
    history.do_command(
        &mut project,
        Box::new(SetClipSpeed::new(
            tl,
            vec![(0, a)],
            Rational::new(2, 1),
            false,
            SpeedFit::Ripple,
        )),
    );
    let clip = project.timelines[tl].clip(0, a).unwrap();
    assert_eq!(clip.media_secs_at(0, 25.0), 2.0);
    assert_eq!(clip.media_secs_at(25, 25.0), 4.0);
}

/// Loading recomputes `rate` from the fps: the speed must survive it.
#[test]
fn refreshing_the_rates_keeps_the_speed() {
    let (mut project, tl, media) = setup(&[TrackKind::Video]);
    let a = add(&mut project, tl, 0, media, (0, 100), 0);
    let mut history = History::default();
    history.do_command(
        &mut project,
        Box::new(SetClipSpeed::new(
            tl,
            vec![(0, a)],
            Rational::new(2, 1),
            false,
            SpeedFit::Ripple,
        )),
    );
    project.refresh_clip_rates();
    assert_eq!(
        project.timelines[tl].clip(0, a).unwrap().rate(),
        Rational::new(1, 2)
    );
}

/// As a trim: the length follows the speed and nothing else moves.
#[test]
fn resize_changes_only_the_clip() {
    let (mut project, tl, media) = setup(&[TrackKind::Video]);
    let a = add(&mut project, tl, 0, media, (0, 100), 0);
    let b = add(&mut project, tl, 0, media, (0, 10), 150);
    let mut history = History::default();
    history.do_command(
        &mut project,
        Box::new(SetClipSpeed::new(
            tl,
            vec![(0, a)],
            Rational::new(2, 1),
            false,
            SpeedFit::Resize,
        )),
    );
    assert_eq!(span(&project, tl, 0, a), (0, 50));
    assert_eq!(span(&project, tl, 0, b), (150, 10));
}

#[test]
fn a_freeze_frame_holds_the_playhead_picture_and_unfreezes_within_the_media() {
    let (mut project, tl, media) = setup(&[TrackKind::Video]);
    let a = add(&mut project, tl, 0, media, (150, 190), 10);
    let mut history = History::default();
    history.do_command(
        &mut project,
        Box::new(SetClipFreeze::new(tl, vec![(0, a)], Some(15))),
    );
    let clip = project.timelines[tl].clip(0, a).unwrap();
    assert_eq!(clip.freeze, Some(155));
    assert_eq!(clip.picture_frame_at(45), 155);
    assert_eq!(clip.source_frame_at(45), 185, "keyframes keep running");
    assert_eq!(
        edit::trim_range(&project, clip, TrimEdge::End).1,
        FrameIdx::MAX,
        "no media bounds a still"
    );

    project.timelines[tl].tracks[0]
        .clip_mut(a)
        .unwrap()
        .timeline_len = 100;
    history.do_command(
        &mut project,
        Box::new(SetClipFreeze::new(tl, vec![(0, a)], None)),
    );
    let clip = project.timelines[tl].clip(0, a).unwrap();
    assert_eq!(clip.freeze, None);
    assert_eq!(
        span(&project, tl, 0, a),
        (10, 50),
        "cut at the end of the media"
    );

    history.undo(&mut project);
    assert_eq!(project.timelines[tl].clip(0, a).unwrap().freeze, Some(155));
}

#[test]
fn a_speed_on_a_frozen_clip_unfreezes_it_within_the_media() {
    let (mut project, tl, media) = setup(&[TrackKind::Video]);
    let a = add(&mut project, tl, 0, media, (150, 190), 10);
    let mut history = History::default();
    history.do_command(
        &mut project,
        Box::new(SetClipFreeze::new(tl, vec![(0, a)], Some(15))),
    );
    project.timelines[tl].tracks[0]
        .clip_mut(a)
        .unwrap()
        .timeline_len = 100;
    history.do_command(
        &mut project,
        Box::new(SetClipSpeed::new(
            tl,
            vec![(0, a)],
            Rational::from_percent(50.0),
            false,
            SpeedFit::Resize,
        )),
    );
    let clip = project.timelines[tl].clip(0, a).unwrap();
    assert_eq!(clip.freeze, None);
    assert_eq!(
        span(&project, tl, 0, a),
        (10, 100),
        "source 150..200 at half speed"
    );
}
