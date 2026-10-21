use super::*;

fn clip_at(timeline_start: FrameIdx, len: FrameIdx, id: u64) -> Clip {
    Clip::from_source_range(
        ClipId(id),
        ClipSource::SolidColor,
        0,
        len,
        timeline_start,
        Rational::one(),
    )
}

#[test]
fn active_video_clip_at_prefers_the_topmost_video_track_where_it_has_a_clip() {
    // Two video tracks: the first (index 0, "bottom") covers [0, 30), the
    // second (index 1, "top") only [10, 20) — a shorter layer
    // overlapping a longer one, the base case of blend-over
    // (plans/REFACTOR_PIPELINE.md B4).
    let tl = Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![
            Track {
                kind: TrackKind::Video,
                clips: vec![clip_at(0, 30, 1)],
                muted: false,
                solo: false,
                locked: false,
                crossings: Vec::new(),
                mix: Default::default(),
                armed: Default::default(),
            },
            Track {
                kind: TrackKind::Video,
                clips: vec![clip_at(10, 10, 2)],
                muted: false,
                solo: false,
                locked: false,
                crossings: Vec::new(),
                mix: Default::default(),
                armed: Default::default(),
            },
        ],
        markers: Vec::new(),
        master: Default::default(),
    };
    assert_eq!(
        tl.active_video_clip_at(5).map(|(t, c)| (t, c.id)),
        Some((0, ClipId(1))),
        "only the bottom track has something here"
    );
    assert_eq!(
        tl.active_video_clip_at(15).map(|(t, c)| (t, c.id)),
        Some((1, ClipId(2))),
        "the top track covers this point: it wins"
    );
    assert_eq!(
        tl.active_video_clip_at(25).map(|(t, c)| (t, c.id)),
        Some((0, ClipId(1))),
        "with the top track uncovered again, the bottom one shows"
    );
    assert!(tl.active_video_clip_at(35).is_none());
}

/// It must be the clip the viewer shows: a disabled clip or a muted track on
/// top lets the one below through, as in `active_video_clips_at`.
#[test]
fn active_video_clip_at_skips_what_is_not_composited() {
    let track = |clip: Clip, muted: bool| Track {
        kind: TrackKind::Video,
        clips: vec![clip],
        muted,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    };
    let mut disabled = clip_at(0, 10, 2);
    disabled.disabled = true;
    let mut tl = Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![track(clip_at(0, 10, 1), false), track(disabled, false)],
        markers: Vec::new(),
        master: Default::default(),
    };
    assert_eq!(
        tl.active_video_clip_at(5).map(|(_, c)| c.id),
        Some(ClipId(1))
    );

    tl.tracks[1] = track(clip_at(0, 10, 3), true);
    assert_eq!(
        tl.active_video_clip_at(5).map(|(_, c)| c.id),
        Some(ClipId(1))
    );
}

#[test]
fn active_video_clips_at_returns_every_covering_track_bottom_to_top() {
    let tl = Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![
            Track {
                kind: TrackKind::Video,
                clips: vec![clip_at(0, 30, 1)],
                muted: false,
                solo: false,
                locked: false,
                crossings: Vec::new(),
                mix: Default::default(),
                armed: Default::default(),
            },
            Track {
                kind: TrackKind::Video,
                clips: vec![clip_at(10, 10, 2)],
                muted: false,
                solo: false,
                locked: false,
                crossings: Vec::new(),
                mix: Default::default(),
                armed: Default::default(),
            },
        ],
        markers: Vec::new(),
        master: Default::default(),
    };
    assert_eq!(
        tl.active_video_clips_at(15)
            .iter()
            .map(|(t, c)| (*t, c.id))
            .collect::<Vec<_>>(),
        vec![(0, ClipId(1)), (1, ClipId(2))],
        "bottom first, top last: it is the compositing order"
    );
    assert_eq!(
        tl.active_video_clips_at(5)
            .iter()
            .map(|(_, c)| c.id)
            .collect::<Vec<_>>(),
        vec![ClipId(1)],
        "here only the bottom track has a clip"
    );
    assert!(tl.active_video_clips_at(35).is_empty());
}

#[test]
fn active_video_clip_at_ignores_audio_tracks() {
    let tl = Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![
            Track {
                kind: TrackKind::Video,
                clips: vec![clip_at(0, 10, 1)],
                muted: false,
                solo: false,
                locked: false,
                crossings: Vec::new(),
                mix: Default::default(),
                armed: Default::default(),
            },
            Track {
                kind: TrackKind::Audio,
                clips: vec![clip_at(0, 10, 2)],
                muted: false,
                solo: false,
                locked: false,
                crossings: Vec::new(),
                mix: Default::default(),
                armed: Default::default(),
            },
        ],
        markers: Vec::new(),
        master: Default::default(),
    };
    assert_eq!(
        tl.active_video_clip_at(5).map(|(_, c)| c.id),
        Some(ClipId(1))
    );
}

#[test]
fn first_track_index_finds_the_bottom_most_track_of_a_kind() {
    let tl = Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![
            Track::new(TrackKind::Video),
            Track::new(TrackKind::Audio),
            Track::new(TrackKind::Video),
        ],
        markers: Vec::new(),
        master: Default::default(),
    };
    assert_eq!(tl.first_track_index(TrackKind::Video), Some(0));
    assert_eq!(tl.first_track_index(TrackKind::Audio), Some(1));
}

#[test]
fn source_frame_at_maps_a_trimmed_clip_correctly() {
    let clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::SolidColor,
        200,
        300,
        60,
        Rational::one(),
    );
    assert_eq!(clip.source_frame_at(60), 200, "first frame of the clip");
    assert_eq!(clip.source_frame_at(75), 215);
}

#[test]
fn total_frames_is_the_furthest_clip_end_across_tracks() {
    let tl = Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![
            Track {
                kind: TrackKind::Video,
                clips: vec![clip_at(0, 10, 1)],
                muted: false,
                solo: false,
                locked: false,
                crossings: Vec::new(),
                mix: Default::default(),
                armed: Default::default(),
            },
            Track {
                kind: TrackKind::Audio,
                clips: vec![clip_at(15, 10, 2)], // ends at 25, later than the video one
                muted: false,
                solo: false,
                locked: false,
                crossings: Vec::new(),
                mix: Default::default(),
                armed: Default::default(),
            },
        ],
        markers: Vec::new(),
        master: Default::default(),
    };
    assert_eq!(tl.total_frames(), 25);
}

fn media_clip_at(
    rate: Rational,
    source_in: FrameIdx,
    source_out: FrameIdx,
    timeline_start: FrameIdx,
) -> Clip {
    Clip::from_source_range(
        ClipId(1),
        ClipSource::SolidColor,
        source_in,
        source_out,
        timeline_start,
        rate,
    )
}

/// 59.94 fps on a 60 fps timeline: the clip lasts on the timeline as long as
/// it really lasts (0.1% more frames), not 1:1 like the source frames.
#[test]
fn a_clip_slower_than_the_timeline_lasts_longer_in_timeline_frames() {
    let rate = Rational::conform_rate(Rational::new(60, 1), Rational::new(60000, 1001));
    assert_eq!(rate, Rational::new(1001, 1000));

    // 15 minutes of source at 59.94 fps.
    let source_len = 53_946;
    let clip = media_clip_at(rate, 0, source_len, 0);
    assert_eq!(clip.source_len(), source_len);
    assert_eq!(clip.timeline_len, 54_000, "exactly 15 minutes at 60 fps");
}

#[test]
fn source_frame_at_maps_the_edges_and_never_drifts_over_fifteen_minutes() {
    let rate = Rational::conform_rate(Rational::new(60, 1), Rational::new(60000, 1001));
    let clip = media_clip_at(rate, 0, 53_946, 120);

    assert_eq!(clip.source_frame_at(120), 0, "first frame of the clip");
    assert_eq!(
        clip.source_frame_at(clip.timeline_end() - 1),
        53_945,
        "last source frame on the last timeline frame"
    );

    // No accumulated drift: at every instant the source frame
    // shown stays the one belonging to the real elapsed time
    // (half a frame of tolerance, the unavoidable rounding).
    for t in (0..clip.timeline_len).step_by(137) {
        let secs = t as f64 / 60.0;
        let expected = secs * (60000.0 / 1001.0);
        let got = clip.source_frame_at(120 + t) as f64;
        assert!(
            (got - expected).abs() <= 0.5,
            "at {secs}s: expected ~{expected}, got {got}"
        );
    }
}

#[test]
fn source_frame_at_maps_a_faster_media_by_skipping_frames() {
    // 50 fps on a 25 fps timeline: two source frames per timeline frame.
    let rate = Rational::conform_rate(Rational::new(25, 1), Rational::new(50, 1));
    assert_eq!(rate, Rational::new(1, 2));
    let clip = media_clip_at(rate, 0, 100, 0);
    assert_eq!(clip.timeline_len, 50);
    assert_eq!(clip.source_frame_at(0), 0);
    assert_eq!(clip.source_frame_at(1), 2);
    assert_eq!(clip.source_frame_at(49), 98);
}

#[test]
fn source_frame_at_maps_a_slower_media_by_repeating_frames() {
    // 25 fps on a 30 fps timeline: 6 timeline frames every 5 source ones.
    let rate = Rational::conform_rate(Rational::new(30, 1), Rational::new(25, 1));
    assert_eq!(rate, Rational::new(6, 5));
    let clip = media_clip_at(rate, 0, 25, 0);
    assert_eq!(clip.timeline_len, 30, "1s a 25 fps dura 1s a 30 fps");
    let sources: Vec<FrameIdx> = (0..6).map(|t| clip.source_frame_at(t)).collect();
    assert_eq!(sources, vec![0, 1, 2, 2, 3, 4], "one frame in six repeated");
}

#[test]
fn rate_one_behaves_exactly_like_before() {
    let clip = media_clip_at(Rational::one(), 200, 300, 60);
    assert_eq!(clip.timeline_len, 100);
    assert_eq!(clip.source_frame_at(60), 200);
    assert_eq!(clip.source_frame_at(75), 215);
    assert_eq!(clip.timeline_frame_at(215), 75);
}

#[test]
fn timeline_frame_at_is_the_inverse_of_source_frame_at() {
    let rate = Rational::conform_rate(Rational::new(60, 1), Rational::new(60000, 1001));
    let clip = media_clip_at(rate, 1_000, 5_000, 300);
    for s in (clip.source_in()..clip.source_out()).step_by(7) {
        assert_eq!(clip.source_frame_at(clip.timeline_frame_at(s)), s);
    }
    assert_eq!(
        clip.timeline_frame_at(clip.source_in()),
        clip.timeline_start
    );
    assert_eq!(
        clip.timeline_frame_at(clip.source_out()),
        clip.timeline_end()
    );
}

#[test]
fn refresh_clip_rates_conforms_a_clip_loaded_without_a_rate() {
    let mut project = Project::default();
    let media_id = project.media_pool.insert(MediaItem {
        path: "/tmp/x.mp4".into(),
        meta: MediaMeta {
            duration_frames: 1000,
            fps: Rational::new(60000, 1001),
            width: 1920,
            height: 1080,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        content_hash: 0,
        compound: None,
        folder: None,
    });
    let mut clip = media_clip_at(Rational::one(), 0, 1000, 0);
    clip.source = ClipSource::Media(media_id);
    let timeline_id = project.timelines.insert(Timeline {
        name: "T".into(),
        fps: Rational::new(60, 1),
        resolution: (1920, 1080),
        tracks: vec![Track {
            kind: TrackKind::Video,
            clips: vec![clip, media_clip_at(Rational::one(), 0, 10, 2000)],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        }],
        markers: Vec::new(),
        master: Default::default(),
    });

    project.refresh_clip_rates();

    let clips = &project.timelines[timeline_id].tracks[0].clips;
    assert_eq!(clips[0].rate, Rational::new(1001, 1000));
    assert_eq!(
        clips[1].rate,
        Rational::one(),
        "a SolidColor does not conform to anything"
    );
    assert_eq!(
        clips[0].timeline_len, 1000,
        "the timeline duration does not change"
    );
}

#[test]
fn retime_keeps_seconds_and_round_trips() {
    let rate_30 = Rational::conform_rate(Rational::new(30, 1), Rational::new(30000, 1001));
    let mut clip = media_clip_at(rate_30, 300, 900, 150);
    let original = (clip.timeline_start, clip.source_offset, clip.timeline_len);

    let rate_25 = Rational::conform_rate(Rational::new(25, 1), Rational::new(30000, 1001));
    clip.retime(Rational::new(30, 1), Rational::new(25, 1), rate_25);
    assert_eq!(clip.timeline_start, 125, "5 secondi");
    assert_eq!(clip.source_offset, 250, "10 seconds into the media");
    assert_eq!(clip.timeline_end(), 626, "end at 751 frames of 30 fps");
    assert_eq!(clip.rate, rate_25);

    clip.retime(Rational::new(25, 1), Rational::new(30, 1), rate_30);
    assert_eq!(
        (clip.timeline_start, clip.source_offset, clip.timeline_len),
        original
    );
}

#[test]
fn total_frames_is_zero_for_an_empty_timeline() {
    let tl = Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    };
    assert_eq!(tl.total_frames(), 0);
}

/// A project saved before a new parameter has fewer tracks: the
/// missing parameter must go back to its default, not make the
/// loading fail.
#[test]
fn transform_tracks_saved_without_a_newer_param_load_with_its_default() {
    let older = "(params: [], flip: (false, false))";
    let tracks: TransformTracks = ron::from_str(older).expect("load");
    let t = tracks.value_at(0);
    assert_eq!(t.opacity, 100.0);
    assert_eq!(t.zoom, [1.0, 1.0]);
}

#[test]
fn a_normalization_saved_before_its_modes_loads_as_a_relative_sample_peak() {
    let older = "(enabled: true, kind: Normalize(target_db: -3.0))";
    let effect: AudioEffect = ron::from_str(older).expect("load");
    assert_eq!(
        effect.kind,
        AudioEffectKind::Normalize {
            target_db: -3.0,
            mode: NormalizeMode::SamplePeak,
            set_level: SetLevel::Relative,
        }
    );
}

fn media_item(path: &str) -> MediaItem {
    MediaItem {
        path: path.into(),
        meta: MediaMeta {
            duration_frames: 10,
            fps: Rational::new(30, 1),
            width: 64,
            height: 48,
            has_video: true,
            has_audio: false,
            sample_rate: 48_000,
            channels: 2,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 0,
        compound: None,
        folder: None,
    }
}

#[test]
fn media_in_folder_includes_the_subfolders() {
    let mut project = Project::default();
    let outer = project.folders.insert(MediaFolder {
        name: "outer".into(),
        parent: None,
    });
    let inner = project.folders.insert(MediaFolder {
        name: "inner".into(),
        parent: Some(outer),
    });
    let in_outer = project.media_pool.insert(MediaItem {
        folder: Some(outer),
        ..media_item("/a.mp4")
    });
    let in_inner = project.media_pool.insert(MediaItem {
        folder: Some(inner),
        ..media_item("/b.mp4")
    });
    project.media_pool.insert(media_item("/root.mp4"));

    let mut media = project.media_in_folder(outer);
    media.sort();
    let mut expected = vec![in_outer, in_inner];
    expected.sort();
    assert_eq!(media, expected);
    assert_eq!(project.media_in_folder(inner), vec![in_inner]);
}

#[test]
fn transitions_do_not_apply_to_adjustment_clips() {
    let transition = Transition {
        kind: TransitionKind::Push,
        duration: 10,
        direction: PushDirection::Right,
        ease: Ease::InOut,
        curve: 0.5,
    };
    let mut adjustment = clip_at(10, 10, 2);
    adjustment.source = ClipSource::Adjustment;
    adjustment.effects.transition_out = Some(transition.clone());
    let mut track = Track::new(TrackKind::Video);
    track.clips = vec![clip_at(0, 10, 1), adjustment];
    track.crossings = vec![CrossTransition {
        left_clip: ClipId(1),
        right_clip: ClipId(2),
        transition,
    }];

    assert!(track.crossing_at(10).is_none());
    assert_eq!(
        track.clips[1].transition_offset_at(18, (1920.0, 1080.0), [1.0, 1.0]),
        [0.0, 0.0]
    );
}

#[test]
fn edge_cross_dissolve_fades_opacity_without_moving_the_clip() {
    let dissolve = Transition {
        kind: TransitionKind::CrossDissolve,
        duration: 10,
        direction: PushDirection::Right,
        ease: Ease::None,
        curve: 0.5,
    };
    let mut clip = clip_at(0, 100, 1);
    clip.effects.transition_in = Some(dissolve.clone());
    clip.effects.transition_out = Some(dissolve);

    assert_eq!(clip.transition_opacity_at(0), 0.0);
    assert_eq!(clip.transition_opacity_at(5), 0.5);
    assert_eq!(clip.transition_opacity_at(50), 1.0);
    assert_eq!(clip.transition_opacity_at(95), 0.5);
    assert_eq!(
        clip.transition_offset_at(5, (1920.0, 1080.0), [1.0, 1.0]),
        [0.0, 0.0]
    );
}

#[test]
fn retime_keeps_the_clip_speed() {
    let rate_30 = Rational::conform_rate(Rational::new(30, 1), Rational::new(30, 1));
    let mut clip = media_clip_at(rate_30, 300, 900, 150);
    clip.set_speed(Rational::new(2, 1), rate_30);

    let rate_25 = Rational::conform_rate(Rational::new(25, 1), Rational::new(30, 1));
    clip.retime(Rational::new(30, 1), Rational::new(25, 1), rate_25);

    assert_eq!(clip.speed(), Rational::new(2, 1));
    assert_eq!(clip.rate(), rate_25.divided_by(Rational::new(2, 1)));
    assert_eq!(clip.conform_rate(), rate_25);
}

#[test]
fn strips_walk_the_track_mixes_in_order_then_the_master() {
    let mut tl = Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Audio), Track::new(TrackKind::Audio)],
        markers: Vec::new(),
        master: Default::default(),
    };
    for (i, strip) in tl.strips_mut().enumerate() {
        strip.gain_db = i as f32;
    }
    let gains: Vec<f32> = tl.strips().map(|s| s.gain_db).collect();
    assert_eq!(gains, [0.0, 1.0, 2.0]);
    assert_eq!(tl.master.gain_db, 2.0);
}
