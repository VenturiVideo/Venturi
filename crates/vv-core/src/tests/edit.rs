use super::*;
use crate::{InsertClip, MediaItem, MediaMeta, SetTrackFlag, Timeline, Track, TrackFlag};

fn project_with_tracks(kinds: &[TrackKind]) -> (Project, TimelineId) {
    let mut project = Project::default();
    let timeline = project.timelines.insert(Timeline {
        name: "Timeline 1".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: kinds.iter().map(|&k| Track::new(k)).collect(),
        markers: Vec::new(),
        master: Default::default(),
    });
    (project, timeline)
}

fn media(project: &mut Project, audio_streams: u16) -> MediaId {
    project.media_pool.insert(MediaItem {
        path: "a.mp4".into(),
        meta: MediaMeta {
            duration_frames: 1000,
            fps: Rational::new(25, 1),
            width: 1920,
            height: 1080,
            has_video: true,
            has_audio: audio_streams > 0,
            sample_rate: 48000,
            channels: 2,
            audio_streams,
            file: Default::default(),
        },
        content_hash: 7,
        compound: None,
        folder: None,
    })
}

fn put(
    project: &mut Project,
    history: &mut History,
    timeline: TimelineId,
    track_index: usize,
    start: FrameIdx,
    len: FrameIdx,
) -> ClipId {
    let id = project.alloc_clip_id();
    let clip = Clip::from_source_range(id, ClipSource::SolidColor, 0, len, start, Rational::one());
    history.do_command(
        project,
        Box::new(InsertClip {
            timeline,
            track_index,
            clip,
        }),
    );
    id
}

/// A 0..100 video clip linked to a 0..100 audio clip.
fn linked_av(project: &mut Project, history: &mut History, timeline: TimelineId) -> [ClipId; 2] {
    let v = put(project, history, timeline, 0, 0, 100);
    let a = put(project, history, timeline, 1, 0, 100);
    history.do_command(
        project,
        Box::new(LinkClips::new(timeline, vec![(0, v), (1, a)])),
    );
    [v, a]
}

fn lock(project: &mut Project, history: &mut History, timeline: TimelineId, track: usize) {
    history.do_command(
        project,
        Box::new(SetTrackFlag::new(timeline, track, TrackFlag::Locked, true)),
    );
}

/// (timeline start, timeline end, source in) of every clip of a track.
fn spans(
    project: &Project,
    timeline: TimelineId,
    track: usize,
) -> Vec<(FrameIdx, FrameIdx, FrameIdx)> {
    project.timelines[timeline].tracks[track]
        .clips
        .iter()
        .map(|c| (c.timeline_start, c.timeline_end(), c.source_in()))
        .collect()
}

fn snapshot(project: &Project, timeline: TimelineId) -> Vec<Vec<(FrameIdx, FrameIdx, FrameIdx)>> {
    (0..project.timelines[timeline].tracks.len())
        .map(|t| spans(project, timeline, t))
        .collect()
}

#[test]
fn split_clips_relinks_the_right_halves_and_skips_locked_tracks() {
    let (mut project, tl) =
        project_with_tracks(&[TrackKind::Video, TrackKind::Audio, TrackKind::Audio]);
    let mut history = History::default();
    let [v, a] = linked_av(&mut project, &mut history, tl);
    put(&mut project, &mut history, tl, 2, 0, 100);
    lock(&mut project, &mut history, tl, 2);

    let split = split_clips(&mut project, &mut history, tl, 40, None);

    let lefts: Vec<ClipRef> = split.iter().map(|&(left, _)| left).collect();
    assert_eq!(lefts, vec![(0, v), (1, a)]);
    assert_eq!(spans(&project, tl, 0), vec![(0, 40, 0), (40, 100, 40)]);
    assert_eq!(spans(&project, tl, 2), vec![(0, 100, 0)]);
    let timeline = &project.timelines[tl];
    let right_v = timeline.tracks[0].clips[1].id;
    let right_a = timeline.tracks[1].clips[1].id;
    assert_eq!(timeline.linked_members(0, right_v), vec![(1, right_a)]);
    assert_eq!(timeline.linked_members(0, v), vec![(1, a)]);
}

#[test]
fn split_clips_restricted_to_only_leaves_the_others_whole() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Audio]);
    let mut history = History::default();
    let v = put(&mut project, &mut history, tl, 0, 0, 100);
    put(&mut project, &mut history, tl, 1, 0, 100);

    let only = BTreeSet::from([(0, v)]);
    split_clips(&mut project, &mut history, tl, 40, Some(&only));

    assert_eq!(spans(&project, tl, 0).len(), 2);
    assert_eq!(spans(&project, tl, 1), vec![(0, 100, 0)]);
}

#[test]
fn delete_clips_keeps_the_clips_of_locked_tracks() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Audio]);
    let mut history = History::default();
    let v = put(&mut project, &mut history, tl, 0, 0, 100);
    let a = put(&mut project, &mut history, tl, 1, 0, 100);
    lock(&mut project, &mut history, tl, 1);

    delete_clips(&mut project, &mut history, tl, &[(0, v), (1, a)]);

    assert!(spans(&project, tl, 0).is_empty());
    assert_eq!(spans(&project, tl, 1), vec![(0, 100, 0)]);
}

#[test]
fn ripple_delete_clips_takes_the_link_group_and_closes_merged_holes() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Audio]);
    let mut history = History::default();
    let [v, _] = linked_av(&mut project, &mut history, tl);
    put(&mut project, &mut history, tl, 0, 100, 50);

    let leftmost = ripple_delete_clips(&mut project, &mut history, tl, &[(0, v)]);

    assert_eq!(leftmost, Some(0));
    assert_eq!(spans(&project, tl, 0), vec![(0, 50, 0)]);
    assert!(spans(&project, tl, 1).is_empty());
}

#[test]
fn delete_ranges_ripple_merges_ranges_and_keeps_av_in_sync() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Audio]);
    let mut history = History::default();
    linked_av(&mut project, &mut history, tl);

    let mark = history.begin_group();
    delete_ranges(
        &mut project,
        &mut history,
        tl,
        &[(50, 60), (10, 20), (15, 30)],
        &RangeDelete::Ripple,
    );
    history.end_group(mark);

    let expected = vec![(0, 10, 0), (10, 30, 30), (30, 70, 60)];
    assert_eq!(spans(&project, tl, 0), expected);
    assert_eq!(spans(&project, tl, 1), expected);
    // Every video piece is still linked to the audio piece beside it.
    let timeline = &project.timelines[tl];
    for (video, audio) in timeline.tracks[0]
        .clips
        .iter()
        .zip(&timeline.tracks[1].clips)
    {
        assert_eq!(timeline.linked_members(0, video.id), vec![(1, audio.id)]);
    }
}

#[test]
fn delete_ranges_is_one_undo_step() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Audio]);
    let mut history = History::default();
    linked_av(&mut project, &mut history, tl);
    let before = snapshot(&project, tl);
    let position = history.position();

    delete_ranges(
        &mut project,
        &mut history,
        tl,
        &[(10, 20), (50, 60)],
        &RangeDelete::Ripple,
    );
    assert_eq!(history.position(), position + 1);
    history.undo(&mut project);

    assert_eq!(snapshot(&project, tl), before);
}

#[test]
fn insert_media_creating_tracks_is_one_undo_step() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Audio]);
    let mut history = History::default();
    let media = media(&mut project, 3);
    let insert = MediaInsert {
        media_id: media,
        source_in: 0,
        source_out: 50,
        video: true,
        audio: true,
    };
    let tracks = TargetTracks {
        video: Some(0),
        extra_audio: None,
    };

    insert_media(&mut project, &mut history, tl, insert, 0, tracks).unwrap();
    assert_eq!(history.position(), 1);
    history.undo(&mut project);

    assert_eq!(project.timelines[tl].tracks.len(), 2);
    assert!(spans(&project, tl, 0).is_empty());
}

#[test]
fn an_enclosing_group_keeps_its_label_and_stays_one_step() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Audio]);
    let mut history = History::default();
    linked_av(&mut project, &mut history, tl);
    let position = history.position();

    let mark = history.begin_group();
    split_clips(&mut project, &mut history, tl, 50, None);
    delete_ranges(
        &mut project,
        &mut history,
        tl,
        &[(10, 20), (70, 80)],
        &RangeDelete::Ripple,
    );
    history.end_group_as(mark, CommandLabel::RippleDelete);

    assert_eq!(history.position(), position + 1);
    assert_eq!(history.labels().last(), Some(CommandLabel::RippleDelete));
}

#[test]
fn delete_ranges_on_clip_edges_does_not_leave_empty_pieces() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video]);
    let mut history = History::default();
    put(&mut project, &mut history, tl, 0, 0, 50);
    put(&mut project, &mut history, tl, 0, 50, 50);

    delete_ranges(
        &mut project,
        &mut history,
        tl,
        &[(50, 100)],
        &RangeDelete::Ripple,
    );

    assert_eq!(spans(&project, tl, 0), vec![(0, 50, 0)]);
}

#[test]
fn delete_ranges_ignores_empty_and_reversed_ranges() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video]);
    let mut history = History::default();
    put(&mut project, &mut history, tl, 0, 0, 100);

    delete_ranges(
        &mut project,
        &mut history,
        tl,
        &[(20, 20), (40, 30)],
        &RangeDelete::Ripple,
    );

    assert_eq!(spans(&project, tl, 0), vec![(0, 100, 0)]);
}

#[test]
fn delete_ranges_lift_on_some_tracks_leaves_the_others_and_the_gap() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Audio]);
    let mut history = History::default();
    put(&mut project, &mut history, tl, 0, 0, 100);
    put(&mut project, &mut history, tl, 1, 0, 100);

    delete_ranges(
        &mut project,
        &mut history,
        tl,
        &[(10, 20)],
        &RangeDelete::Lift {
            tracks: Some(vec![1]),
        },
    );

    assert_eq!(spans(&project, tl, 0), vec![(0, 100, 0)]);
    assert_eq!(spans(&project, tl, 1), vec![(0, 10, 0), (20, 100, 20)]);
}

#[test]
fn delete_ranges_does_not_touch_locked_tracks() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Audio]);
    let mut history = History::default();
    put(&mut project, &mut history, tl, 0, 0, 100);
    put(&mut project, &mut history, tl, 1, 0, 100);
    lock(&mut project, &mut history, tl, 1);

    delete_ranges(
        &mut project,
        &mut history,
        tl,
        &[(10, 20)],
        &RangeDelete::Lift { tracks: None },
    );

    assert_eq!(spans(&project, tl, 0), vec![(0, 10, 0), (20, 100, 20)]);
    assert_eq!(spans(&project, tl, 1), vec![(0, 100, 0)]);
}

#[test]
fn insert_media_creates_an_audio_track_per_stream_and_links_everything() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Audio]);
    let mut history = History::default();
    let media = media(&mut project, 2);

    let clips = insert_media(
        &mut project,
        &mut history,
        tl,
        MediaInsert {
            media_id: media,
            source_in: 0,
            source_out: 50,
            video: true,
            audio: true,
        },
        10,
        TargetTracks {
            video: Some(0),
            extra_audio: None,
        },
    )
    .unwrap();

    assert_eq!(project.timelines[tl].tracks.len(), 3);
    assert_eq!(clips.len(), 3);
    let timeline = &project.timelines[tl];
    let (video_track, video_id) = clips[0];
    assert_eq!(timeline.linked_members(video_track, video_id).len(), 2);
    let streams: Vec<usize> = clips[1..]
        .iter()
        .map(|&(t, id)| timeline.clip(t, id).unwrap().audio_stream_index)
        .collect();
    assert_eq!(streams, vec![0, 1]);
}

#[test]
fn inserting_video_uses_free_audio_tracks_without_replacing_existing_clips() {
    let (mut project, tl) =
        project_with_tracks(&[TrackKind::Video, TrackKind::Audio, TrackKind::Audio]);
    let mut history = History::default();
    let existing = put(&mut project, &mut history, tl, 1, 0, 100);
    let media_id = media(&mut project, 2);
    let before = snapshot(&project, tl);
    let position = history.position();
    let clips = insert_media(
        &mut project,
        &mut history,
        tl,
        MediaInsert {
            media_id,
            source_in: 0,
            source_out: 50,
            video: true,
            audio: true,
        },
        10,
        TargetTracks {
            video: Some(0),
            extra_audio: None,
        },
    )
    .unwrap();
    assert_eq!(project.timelines[tl].tracks[1].clips[0].id, existing);
    assert_eq!(spans(&project, tl, 1), vec![(0, 100, 0)]);
    assert_eq!(
        clips.iter().map(|(track, _)| *track).collect::<Vec<_>>(),
        vec![0, 2, 3]
    );
    assert_eq!(history.position(), position + 1);
    history.undo(&mut project);
    assert_eq!(snapshot(&project, tl), before);
    assert_eq!(project.timelines[tl].tracks.len(), 3);
    history.redo(&mut project);
    assert_eq!(spans(&project, tl, 1), vec![(0, 100, 0)]);
    assert_eq!(project.timelines[tl].tracks.len(), 4);
}

#[test]
fn inserting_video_reuses_nonoverlapping_audio_but_skips_locked_tracks() {
    let (mut project, tl) =
        project_with_tracks(&[TrackKind::Video, TrackKind::Audio, TrackKind::Audio]);
    let mut history = History::default();
    lock(&mut project, &mut history, tl, 1);
    put(&mut project, &mut history, tl, 2, 0, 10);
    let media_id = media(&mut project, 1);
    let clips = insert_media(
        &mut project,
        &mut history,
        tl,
        MediaInsert {
            media_id,
            source_in: 0,
            source_out: 50,
            video: true,
            audio: true,
        },
        10,
        TargetTracks {
            video: Some(0),
            extra_audio: None,
        },
    )
    .unwrap();
    assert_eq!(clips[1].0, 2);
    assert_eq!(spans(&project, tl, 2), vec![(0, 10, 0), (10, 60, 0)]);
    assert_eq!(project.timelines[tl].tracks.len(), 3);
}

#[test]
fn an_explicit_first_audio_destination_does_not_overwrite_other_streams() {
    let (mut project, tl) =
        project_with_tracks(&[TrackKind::Video, TrackKind::Audio, TrackKind::Audio]);
    let mut history = History::default();
    put(&mut project, &mut history, tl, 1, 0, 100);
    let media_id = media(&mut project, 2);
    let clips = insert_media(
        &mut project,
        &mut history,
        tl,
        MediaInsert {
            media_id,
            source_in: 0,
            source_out: 50,
            video: true,
            audio: true,
        },
        10,
        TargetTracks {
            video: Some(0),
            extra_audio: Some(2),
        },
    )
    .unwrap();
    assert_eq!(
        clips.iter().map(|(track, _)| *track).collect::<Vec<_>>(),
        vec![0, 2, 3]
    );
    assert_eq!(spans(&project, tl, 1), vec![(0, 100, 0)]);
}

#[test]
fn insert_media_of_an_unknown_media_does_nothing() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video]);
    let mut history = History::default();
    let media = media(&mut project, 0);
    project.media_pool.remove(media);

    let result = insert_media(
        &mut project,
        &mut history,
        tl,
        MediaInsert {
            media_id: media,
            source_in: 0,
            source_out: 50,
            video: true,
            audio: true,
        },
        0,
        TargetTracks {
            video: Some(0),
            extra_audio: None,
        },
    );

    assert!(result.is_none());
    assert!(spans(&project, tl, 0).is_empty());
}

#[test]
fn insert_generator_overwrites_what_is_underneath() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video]);
    let mut history = History::default();
    put(&mut project, &mut history, tl, 0, 0, 500);

    let id = insert_generator(
        &mut project,
        &mut history,
        tl,
        Generator::SolidColor,
        0,
        100,
        None,
    );

    assert_eq!(
        spans(&project, tl, 0),
        vec![(0, 100, 0), (100, 225, 0), (225, 500, 225)]
    );
    let clip = project.timelines[tl].clip(0, id).unwrap();
    assert_eq!(
        clip.effects.color.as_ref().map(|c| c.value_at(0)),
        Some(DEFAULT_SOLID_COLOR)
    );
}

fn place(
    project: &mut Project,
    history: &mut History,
    tl: TimelineId,
    media: MediaId,
    source: (FrameIdx, FrameIdx),
    at: FrameIdx,
    audio: bool,
) {
    insert_media(
        project,
        history,
        tl,
        MediaInsert {
            media_id: media,
            source_in: source.0,
            source_out: source.1,
            video: true,
            audio,
        },
        at,
        TargetTracks {
            video: Some(0),
            extra_audio: None,
        },
    )
    .unwrap();
}

#[test]
fn media_ranges_follow_the_material_after_earlier_cuts() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Audio]);
    let mut history = History::default();
    let media = media(&mut project, 1);
    place(&mut project, &mut history, tl, media, (0, 100), 0, true);
    delete_ranges(
        &mut project,
        &mut history,
        tl,
        &[(10, 20)],
        &RangeDelete::Ripple,
    );

    let removed = delete_media_ranges(
        &mut project,
        &mut history,
        tl,
        media,
        &[(30, 40)],
        &RangeDelete::Ripple,
    );

    assert_eq!(removed, [(0, 20, 30), (1, 20, 30)]);
    let expected = vec![(0, 10, 0), (10, 20, 20), (20, 80, 40)];
    assert_eq!(spans(&project, tl, 0), expected);
    assert_eq!(spans(&project, tl, 1), expected);
}

#[test]
fn a_media_used_twice_loses_the_frames_in_both_places() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video]);
    let mut history = History::default();
    let media = media(&mut project, 0);
    place(&mut project, &mut history, tl, media, (0, 50), 0, false);
    place(&mut project, &mut history, tl, media, (0, 50), 100, false);

    delete_media_ranges(
        &mut project,
        &mut history,
        tl,
        media,
        &[(10, 20)],
        &RangeDelete::Lift { tracks: None },
    );

    let starts: Vec<(FrameIdx, FrameIdx)> = spans(&project, tl, 0)
        .iter()
        .map(|&(s, e, _)| (s, e))
        .collect();
    assert_eq!(starts, [(0, 10), (20, 50), (100, 110), (120, 150)]);
}

#[test]
fn lifting_media_ranges_leaves_other_material_at_the_same_time() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Video]);
    let mut history = History::default();
    let media = media(&mut project, 0);
    place(&mut project, &mut history, tl, media, (0, 100), 0, false);
    put(&mut project, &mut history, tl, 1, 0, 100);

    delete_media_ranges(
        &mut project,
        &mut history,
        tl,
        media,
        &[(10, 20)],
        &RangeDelete::Lift { tracks: None },
    );

    assert_eq!(spans(&project, tl, 0), [(0, 10, 0), (20, 100, 20)]);
    assert_eq!(spans(&project, tl, 1), [(0, 100, 0)]);
}

#[test]
fn lifting_media_ranges_on_several_tracks_is_one_undo_step() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Audio]);
    let mut history = History::default();
    let media = media(&mut project, 1);
    place(&mut project, &mut history, tl, media, (0, 100), 0, true);
    let before = snapshot(&project, tl);
    let position = history.position();

    delete_media_ranges(
        &mut project,
        &mut history,
        tl,
        media,
        &[(10, 20)],
        &RangeDelete::Lift { tracks: None },
    );
    assert_eq!(spans(&project, tl, 1), [(0, 10, 0), (20, 100, 20)]);
    assert_eq!(history.position(), position + 1);
    history.undo(&mut project);

    assert_eq!(snapshot(&project, tl), before);
}

#[test]
fn media_ranges_map_through_a_conformed_frame_rate() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video]);
    let mut history = History::default();
    let media = media(&mut project, 0);
    project.media_pool[media].meta.fps = Rational::new(30000, 1001);
    place(&mut project, &mut history, tl, media, (0, 300), 0, false);
    let clip = project.timelines[tl].tracks[0].clips[0].clone();

    let mapped = media_ranges_on_timeline(&project, tl, media, &[(100, 130)], None);

    assert_eq!(
        mapped,
        [(0, clip.timeline_frame_at(100), clip.timeline_frame_at(130))]
    );
    assert_ne!(mapped[0].1, 100, "25 fps timeline, 29.97 fps media");
}

#[test]
fn media_ranges_outside_the_used_portion_remove_nothing() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video]);
    let mut history = History::default();
    let media = media(&mut project, 0);
    place(&mut project, &mut history, tl, media, (100, 200), 0, false);
    let before = history.position();

    let removed = delete_media_ranges(
        &mut project,
        &mut history,
        tl,
        media,
        &[(0, 50), (300, 400)],
        &RangeDelete::Ripple,
    );

    assert!(removed.is_empty());
    assert_eq!(history.position(), before);
}

#[test]
fn a_generator_grows_before_its_first_frame_and_keeps_its_keyframes() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video]);
    let mut history = History::default();
    let id = put(&mut project, &mut history, tl, 0, 50, 100);
    let clip = project.timelines[tl].clip_mut(0, id).unwrap();
    clip.effects
        .transform
        .track_mut(crate::TransformParam::Opacity)
        .upsert(10, 40.0, crate::Interpolation::Linear);
    let clip = project.timelines[tl].clip(0, id).unwrap();
    assert_eq!(trim_range(&project, clip, TrimEdge::Start).0, 0);

    trim_clip(&mut project, &mut history, tl, (0, id), TrimEdge::Start, 20);
    let clip = project.timelines[tl].clip(0, id).unwrap();
    assert_eq!(
        (clip.timeline_start, clip.source_offset, clip.timeline_len),
        (20, 0, 130)
    );
    let opacity = clip.effects.transform.track(crate::TransformParam::Opacity);
    assert_eq!(opacity.keyframes()[0].0, 40, "still at timeline frame 60");

    history.undo(&mut project);
    let clip = project.timelines[tl].clip(0, id).unwrap();
    assert_eq!((clip.timeline_start, clip.source_offset), (50, 0));
    let opacity = clip.effects.transform.track(crate::TransformParam::Opacity);
    assert_eq!(opacity.keyframes()[0].0, 10);
}

#[test]
fn a_media_clip_still_stops_at_its_first_frame() {
    let (mut project, _) = project_with_tracks(&[TrackKind::Video]);
    let media_id = media(&mut project, 0);
    let clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(media_id),
        10,
        100,
        50,
        Rational::one(),
    );
    assert_eq!(trim_range(&project, &clip, TrimEdge::Start).0, 40);
}

fn put_media(
    project: &mut Project,
    history: &mut History,
    timeline: TimelineId,
    track_index: usize,
    media_id: MediaId,
    source: (FrameIdx, FrameIdx),
    start: FrameIdx,
) -> ClipId {
    let id = project.alloc_clip_id();
    let clip = Clip::from_source_range(
        id,
        ClipSource::Media(media_id),
        source.0,
        source.1,
        start,
        Rational::one(),
    );
    history.do_command(
        project,
        Box::new(InsertClip {
            timeline,
            track_index,
            clip,
        }),
    );
    id
}

#[test]
fn slip_range_is_the_media_left_on_each_side() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video]);
    let mut history = History::default();
    let media_id = media(&mut project, 0);
    let id = put_media(&mut project, &mut history, tl, 0, media_id, (100, 300), 50);
    let clip = project.timelines[tl].clip(0, id).unwrap();
    assert_eq!(slip_range(&project, clip), Some((-100, 700)));

    let generator = put(&mut project, &mut history, tl, 0, 400, 10);
    let clip = project.timelines[tl].clip(0, generator).unwrap();
    assert_eq!(slip_range(&project, clip), None);
}

#[test]
fn slip_clips_moves_the_content_of_the_whole_group_and_undoes() {
    let (mut project, tl) = project_with_tracks(&[TrackKind::Video, TrackKind::Audio]);
    let mut history = History::default();
    let media_id = media(&mut project, 1);
    let v = put_media(&mut project, &mut history, tl, 0, media_id, (100, 300), 50);
    let a = put_media(&mut project, &mut history, tl, 1, media_id, (100, 300), 50);
    let before = snapshot(&project, tl);

    slip_clips(&mut project, &mut history, tl, &[(0, v), (1, a)], -40);

    assert_eq!(spans(&project, tl, 0), vec![(50, 250, 60)]);
    assert_eq!(spans(&project, tl, 1), vec![(50, 250, 60)]);
    history.undo(&mut project);
    assert_eq!(snapshot(&project, tl), before);
}
