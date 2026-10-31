use super::*;
use vv_core::{Clip, ClipId, MediaItem, MediaMeta, Rational, Track, TrackKind};
use vv_media::HwPriority;

fn make_test_clip(
    dir_name: &str,
    file_name: &str,
    duration_secs: u32,
) -> (vv_media::test_support::TempDir, std::path::PathBuf) {
    let dir = vv_media::test_support::TempDir::new(dir_name);
    let path = dir.join(file_name);
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size=320x240:rate=25:duration={duration_secs}"),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );
    (dir, path)
}

fn make_test_image(
    dir_name: &str,
    file_name: &str,
) -> (vv_media::test_support::TempDir, std::path::PathBuf) {
    let dir = vv_media::test_support::TempDir::new(dir_name);
    let path = dir.join(file_name);
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=green:size=320x240:rate=1:duration=1",
            "-frames:v",
            "1",
            "-update",
            "1",
        ],
        &path,
    );
    (dir, path)
}

/// End-to-end regression for image support
/// (`Decoder::open_image`, `position_decoder`): a still image
/// stretched over a 2.4s clip at 25fps asks for source frames up to
/// ~60 — well past the single real frame an image has. Before the
/// dedicated support, a plain `Decoder::open` would have hit EOF
/// on any position past the first, leaving the cache uncovered
/// for the rest of the clip.
#[test]
fn walk_and_fill_decodes_a_stretched_image_clip_past_its_only_real_frame() {
    let (_dir_path, path) = make_test_image("vv-app-render-ahead-image-test", "still.png");
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: vv_core::IMAGE_DURATION_FRAMES,
            fps: vv_media::IMAGE_FPS,
            width: 320,
            height: 240,
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
    // 60 frames (2.4s at 25fps): within `DEFAULT_LOOKAHEAD_SECS`
    // (3s), otherwise the last frame would stay outside the window
    // for a reason independent of this test (the lookahead, not the
    // image support).
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 60)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let generous_budget = 320 * 240 * 4 * 200;
    let outcome = walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        0,
        generous_budget,
        false,
        None,
        1,
        DEFAULT_LOOKAHEAD_SECS,
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(0),
    );

    assert!(!outcome.interrupted);
    for frame in [0, 1, 30, 59] {
        assert!(
            caches.get(media_a, frame).is_some(),
            "source frame {frame} of the image was not decoded"
        );
    }
}

/// Like `make_test_clip`, but with a short and explicit GOP: without
/// this, the keyframe nearest to a target far from the start
/// is still the initial one (default keyint 250, longer
/// than the duration of the test clips), so a seek further into
/// the file would still have to cross in sequence everything
/// preceding it — masking a possible incorrect eviction of a
/// portion already buffered, because it would be regenerated anyway
/// on the way. With a short GOP the seek can jump directly
/// near the target without touching the preceding portions already
/// cached, making an undue eviction visible.
fn make_test_clip_with_short_gop(
    dir_name: &str,
    file_name: &str,
    duration_secs: u32,
    gop: u32,
) -> (vv_media::test_support::TempDir, std::path::PathBuf) {
    let dir = vv_media::test_support::TempDir::new(dir_name);
    let path = dir.join(file_name);
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size=320x240:rate=25:duration={duration_secs}"),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-g",
            &gop.to_string(),
            "-keyint_min",
            &gop.to_string(),
        ],
        &path,
    );
    (dir, path)
}

/// Enough for the pure tests of `collect_media_segments`, which do not
/// need a real `MediaItem` behind them.
fn dummy_media_item() -> MediaItem {
    MediaItem {
        path: "dummy.mp4".into(),
        meta: MediaMeta {
            duration_frames: 0,
            fps: Rational::new(25, 1),
            width: 0,
            height: 0,
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
    }
}

fn two_media_ids() -> (MediaId, MediaId) {
    let mut project = Project::default();
    let a = project.media_pool.insert(dummy_media_item());
    let b = project.media_pool.insert(dummy_media_item());
    (a, b)
}

/// Dummy 1x1 frame: enough to populate `SharedFrameCache` in the
/// pure tests of `without_already_cached_chunks`, which only check
/// which indices come out covered, never the actual content.
fn dummy_frame() -> FrameYuv420 {
    FrameYuv420 {
        width: 1,
        height: 1,
        y: vec![0],
        chroma: vv_media::Chroma::Planar {
            u: vec![0],
            v: vec![0],
        },
        chroma_width: 1,
        chroma_height: 1,
        matrix: vv_media::ColorMatrix::Bt601,
        full_range: false,
        alpha: None,
    }
}

fn media_clip(id: u64, media_id: MediaId, start: FrameIdx, len: FrameIdx) -> Clip {
    Clip::from_source_range(
        ClipId(id),
        ClipSource::Media(media_id),
        0,
        len,
        start,
        Rational::one(),
    )
}

/// Like `media_clip`, but with an explicit `source_in` — used to
/// simulate two clips on the timeline that are *cuts* of the same
/// long file (sequential source ranges, not both from 0), the
/// common case that exposes the per-segment `evict_before` bug.
fn media_clip_trimmed(
    id: u64,
    media_id: MediaId,
    timeline_start: FrameIdx,
    source_in: FrameIdx,
    len: FrameIdx,
) -> Clip {
    Clip::from_source_range(
        ClipId(id),
        ClipSource::Media(media_id),
        source_in,
        source_in + len,
        timeline_start,
        Rational::one(),
    )
}

fn solid_clip(id: u64, start: FrameIdx, len: FrameIdx) -> Clip {
    Clip::from_source_range(
        ClipId(id),
        ClipSource::SolidColor,
        0,
        len,
        start,
        Rational::one(),
    )
}

fn timeline_with(tracks: Vec<Track>) -> Timeline {
    Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (320, 240),
        tracks,
        markers: Vec::new(),
        master: Default::default(),
    }
}

#[test]
fn collect_media_segments_walks_across_a_straight_cut_between_two_media() {
    let (media_a, media_b) = two_media_ids();
    let tl = timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![
            media_clip(1, media_a, 0, 50),
            media_clip(2, media_b, 50, 50),
        ],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);

    let segments = collect_media_segments(&Project::default(), &tl, 40, 60);
    assert_eq!(segments.len(), 2, "must cross the cut in one go");
    assert_eq!(segments[0].source_start, 40);
    assert_eq!(segments[0].source_end, 49);
    assert_eq!(segments[1].source_start, 0);
    assert_eq!(segments[1].source_end, 9);
}

#[test]
fn collect_media_segments_skips_gaps_and_solid_color_without_decoding() {
    let (media_a, _) = two_media_ids();
    let tl = timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![
            media_clip(1, media_a, 0, 10),
            // gap 10..20
            solid_clip(2, 20, 10),
            media_clip(3, media_a, 30, 10),
        ],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);

    let segments = collect_media_segments(&Project::default(), &tl, 0, 40);
    assert_eq!(
        segments.len(),
        2,
        "only the two Media clips produce segments"
    );
    assert_eq!((segments[0].source_start, segments[0].source_end), (0, 9));
    assert_eq!((segments[1].source_start, segments[1].source_end), (0, 9));
}

/// With several video tracks the buffer must cover them all, not only the
/// top one: under the bars of a clip with an aspect different from the
/// timeline's, the layer below shows, so it must be decoded.
#[test]
fn collect_media_segments_covers_every_video_track_topmost_first() {
    let (media_a, media_b) = two_media_ids();
    let tl = timeline_with(vec![
        Track {
            kind: TrackKind::Video,
            clips: vec![media_clip(1, media_a, 0, 50)],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        },
        Track {
            kind: TrackKind::Video,
            clips: vec![media_clip(2, media_b, 0, 50)],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        },
    ]);

    let segments = collect_media_segments(&Project::default(), &tl, 0, 50);
    assert_eq!(segments.len(), 2);
    assert_eq!(
        (segments[0].media_id, segments[1].media_id),
        (media_b, media_a),
        "at equal position the topmost track takes priority"
    );
}

#[test]
fn collect_media_segments_is_empty_for_a_timeline_with_no_clips() {
    let tl = timeline_with(vec![Track::new(TrackKind::Video)]);
    assert!(collect_media_segments(&Project::default(), &tl, 0, 100).is_empty());
}

#[test]
fn collect_media_segments_behind_walks_across_a_straight_cut_between_two_media() {
    let (media_a, media_b) = two_media_ids();
    let tl = timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![
            media_clip(1, media_a, 0, 50),
            media_clip(2, media_b, 50, 50),
        ],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);

    // Window behind [40,60): crosses the cut at 50 going
    // backwards, symmetric to the forward test above.
    let segments = collect_media_segments_behind(&Project::default(), &tl, 60, 40);
    assert_eq!(segments.len(), 2, "must cross the cut backwards in one go");
    // Discovery order: from the nearest to the playhead (60) to the
    // farthest — first the piece of media_b [50,60), then the one of
    // media_a [40,50).
    assert_eq!(segments[0].source_start, 0);
    assert_eq!(segments[0].source_end, 9);
    assert_eq!(segments[1].source_start, 40);
    assert_eq!(segments[1].source_end, 49);
}

#[test]
fn collect_media_segments_behind_skips_gaps_and_solid_color_without_decoding() {
    let (media_a, _) = two_media_ids();
    let tl = timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![
            media_clip(1, media_a, 0, 10),
            // gap 10..20
            solid_clip(2, 20, 10),
            media_clip(3, media_a, 30, 10),
        ],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);

    let segments = collect_media_segments_behind(&Project::default(), &tl, 40, 0);
    assert_eq!(
        segments.len(),
        2,
        "only the two Media clips produce segments, the gap and the SolidColor are skipped"
    );
    assert_eq!((segments[0].source_start, segments[0].source_end), (0, 9));
    assert_eq!((segments[1].source_start, segments[1].source_end), (0, 9));
}

#[test]
fn collect_media_segments_behind_is_empty_for_a_timeline_with_no_clips() {
    let tl = timeline_with(vec![Track::new(TrackKind::Video)]);
    assert!(collect_media_segments_behind(&Project::default(), &tl, 100, 0).is_empty());
}

#[test]
fn collect_media_segments_behind_stops_at_the_start_frame_bound() {
    // A single long clip [0,200): the window behind must stop
    // exactly at `start_frame`, not continue to the start
    // of the clip.
    let (media_a, _) = two_media_ids();
    let tl = timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 200)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);

    let segments = collect_media_segments_behind(&Project::default(), &tl, 150, 100);
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].timeline_start, 100);
    assert_eq!(segments[0].source_start, 100);
    assert_eq!(segments[0].source_end, 149);
}

/// A project with a compound clip: its real media (inside the
/// nested timeline) must show up among the segments to decode,
/// in the space of the nested timeline — not in that of the outer
/// timeline — while the compound clip itself never shows up (it is not
/// decoded: it is composed on the fly, see `frame_provider::GpuCompounds`).
fn project_with_compound_clip() -> (Project, Timeline, MediaId, MediaId) {
    let mut project = Project::default();
    let real_media = project.media_pool.insert(dummy_media_item());
    let nested = project.timelines.insert(Timeline {
        name: "Nested".into(),
        fps: Rational::new(25, 1),
        resolution: (320, 240),
        tracks: vec![Track {
            kind: TrackKind::Video,
            clips: vec![media_clip(100, real_media, 0, 40)],
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
    let compound_media = project.media_pool.insert(vv_core::MediaItem {
        path: "Compound Clip 1".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 40,
            fps: Rational::new(25, 1),
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
        compound: Some(nested),
        folder: None,
    });
    let root = timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, compound_media, 0, 40)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);
    (project, root, real_media, compound_media)
}

#[test]
fn collect_media_segments_recurses_into_a_compound_clips_nested_timeline() {
    let (project, root, real_media, _) = project_with_compound_clip();

    let real = collect_media_segments(&project, &root, 10, 30);

    assert_eq!(
        real.len(),
        1,
        "only the real media inside the compound clip, the only one to decode"
    );
    assert_eq!(real[0].media_id, real_media);
    assert_eq!(
        real[0].source_start, 10,
        "same range, in frames of the nested timeline"
    );
    assert_eq!(real[0].source_end, 29);
}

/// The nested segments are placed on the root timeline: the cache measures
/// their distance from the root playhead, and the chunks interleave with the
/// root's own media by that position.
#[test]
fn collect_media_segments_places_a_compound_clips_segments_on_the_root_timeline() {
    let (project, mut root, real_media, compound_media) = project_with_compound_clip();
    root.tracks[0].clips = vec![media_clip_trimmed(1, compound_media, 100, 0, 40)];

    let real = collect_media_segments(&project, &root, 110, 130);

    assert_eq!(real[0].media_id, real_media);
    assert_eq!(real[0].source_start, 10);
    assert_eq!(
        real[0].timeline_start, 110,
        "not 10, its frame in the nested timeline"
    );
}

/// Since the project timeline shows up in the media pool too
/// (see `MediaItem::compound`), a user can drag it inside
/// itself: a cycle, not just a deep nesting. Without
/// `MAX_COMPOUND_DEPTH` this would stack overflow instead of
/// stopping.
#[test]
fn collect_media_segments_stops_at_a_cyclic_compound_clip_instead_of_overflowing() {
    let mut project = Project::default();
    let timeline_id = project.timelines.insert(timeline_with(vec![]));
    let media_id = project.media_pool.insert(vv_core::MediaItem {
        path: "Timeline 1".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 100,
            fps: Rational::new(25, 1),
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
        compound: Some(timeline_id),
        folder: None,
    });
    // The timeline references itself through its own entry in the pool.
    project.timelines[timeline_id].tracks.push(Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_id, 0, 100)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    });

    let real = collect_media_segments(&project, &project.timelines[timeline_id], 0, 100);

    assert!(real.is_empty(), "no real media to decode in a pure cycle");
}

/// Two clips playing together on two video tracks must be filled
/// interleaved chunk by chunk, not one whole clip then the other.
#[test]
fn chunk_forward_segments_near_to_far_interleaves_two_overlapping_tracks() {
    let (media_a, media_b) = two_media_ids();
    let segments = vec![
        WantedRange {
            media_id: media_a,
            source_start: 0,
            source_end: 44,
            timeline_start: 100,
            rate: Rational::new(1, 1),
        },
        WantedRange {
            media_id: media_b,
            source_start: 0,
            source_end: 44,
            timeline_start: 100,
            rate: Rational::new(1, 1),
        },
    ];

    let chunks = chunk_forward_segments_near_to_far(&segments);

    let order: Vec<(MediaId, FrameIdx, FrameIdx)> = chunks
        .iter()
        .map(|c| (c.media_id, c.source_start, c.source_end))
        .collect();
    assert_eq!(
        order,
        vec![
            (media_a, 0, 14),
            (media_b, 0, 14),
            (media_a, 15, 29),
            (media_b, 15, 29),
            (media_a, 30, 44),
            (media_b, 30, 44),
        ]
    );
}

/// A segment behind the playhead longer than `BEHIND_CHUNK_FRAMES`
/// must be split into chunks from the near edge (`source_end`) to the
/// far edge (`source_start`), each at most `BEHIND_CHUNK_FRAMES` long,
/// with no holes nor overlaps — see the docs of
/// `chunk_behind_segments_near_to_far`.
#[test]
fn chunk_behind_segments_near_to_far_splits_from_the_near_edge_without_gaps() {
    let (media_a, _) = two_media_ids();
    let segment = WantedRange {
        media_id: media_a,
        source_start: 100,
        source_end: 132, // 33 frames: 2 chunks of 15 + 1 of 3
        timeline_start: 500,
        rate: Rational::one(),
    };

    let chunks = chunk_behind_segments_near_to_far(&[segment]);

    assert_eq!(
        chunks
            .iter()
            .map(|c| (c.source_start, c.source_end))
            .collect::<Vec<_>>(),
        vec![(118, 132), (103, 117), (100, 102)],
        "from the near edge (132) to the far one (100), each at most BEHIND_CHUNK_FRAMES"
    );
    // `timeline_start` follows the same offset as `source_start`
    // relative to the original segment (affine mapping, see the docs).
    assert_eq!(chunks[0].timeline_start, 518);
    assert_eq!(chunks[1].timeline_start, 503);
    assert_eq!(chunks[2].timeline_start, 500);
}

/// A segment shorter than a chunk produces a single chunk
/// identical to the original segment — no superfluous splitting.
#[test]
fn chunk_behind_segments_near_to_far_keeps_a_short_segment_whole() {
    let (media_a, _) = two_media_ids();
    let segment = WantedRange {
        media_id: media_a,
        source_start: 40,
        source_end: 44,
        timeline_start: 40,
        rate: Rational::one(),
    };

    let chunks = chunk_behind_segments_near_to_far(&[segment]);

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].source_start, 40);
    assert_eq!(chunks[0].source_end, 44);
}

/// `without_already_cached_chunks` must discard only the chunks
/// entirely covered by a *single* cached interval — an
/// uncovered or only partially covered chunk stays in the list
/// (rechecking in that case is cheap anyway, see the docs of the
/// function).
#[test]
fn without_already_cached_chunks_drops_only_fully_covered_chunks() {
    let (media_a, media_b) = two_media_ids();
    let caches = SharedFrameCache::new();
    // media_a: [100,132] entirely cached (a single dummy frame
    // for each index, just to populate `cached_ranges`).
    for idx in 100..=132 {
        caches.insert(media_a, idx, Arc::new(dummy_frame()));
    }
    // media_b: only [100,110] cached, not the whole requested chunk.
    for idx in 100..=110 {
        caches.insert(media_b, idx, Arc::new(dummy_frame()));
    }

    let chunks = vec![
        // media_a: entirely covered, must be discarded.
        WantedRange {
            media_id: media_a,
            source_start: 118,
            source_end: 132,
            timeline_start: 118,
            rate: Rational::one(),
        },
        // media_b: only partially covered, stays.
        WantedRange {
            media_id: media_b,
            source_start: 95,
            source_end: 110,
            timeline_start: 95,
            rate: Rational::one(),
        },
        // media_a: outside the cached interval, stays.
        WantedRange {
            media_id: media_a,
            source_start: 50,
            source_end: 64,
            timeline_start: 50,
            rate: Rational::one(),
        },
    ];

    let remaining = without_already_cached_chunks(&caches, chunks);

    assert_eq!(remaining.len(), 2);
    assert_eq!(remaining[0].media_id, media_b);
    assert_eq!(remaining[1].source_start, 50);
}

/// End-to-end test: the worker crosses a hard cut between two
/// different media in a single lookahead window, buffering
/// *both* without needing any special case — exactly the
/// required behavior ("buffer at timeline level, not at single
/// clip level").
#[test]
fn render_ahead_buffers_across_a_straight_cut_between_two_different_media() {
    let (_dir_path_a, path_a) = make_test_clip("vv-app-render-ahead-test", "a.mp4", 2);
    let (_dir_path_b, path_b) = make_test_clip("vv-app-render-ahead-test", "b.mp4", 2);

    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path: path_a,
        meta: MediaMeta {
            duration_frames: 50,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let media_b = project.media_pool.insert(MediaItem {
        path: path_b,
        meta: MediaMeta {
            duration_frames: 50,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![
            media_clip(1, media_a, 0, 50),  // [0,50)
            media_clip(2, media_b, 50, 50), // [50,100), adjacent
        ],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let render_ahead = RenderAhead::spawn(
        project,
        timeline_id,
        100_000_000,
        None,
        DEFAULT_LOOKAHEAD_SECS,
        DEFAULT_BEHIND_SECS,
    );
    // Target near the end of the first clip: the lookahead
    // window (3s = 75 frames at 25fps) amply crosses the
    // cut at 50.
    render_ahead.set_target(45);

    let start = std::time::Instant::now();
    loop {
        let a_ready = !render_ahead.cached_ranges_for(media_a).is_empty();
        let b_ready = !render_ahead.cached_ranges_for(media_b).is_empty();
        if a_ready && b_ready {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout: a_ready={a_ready} b_ready={b_ready}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// End-to-end reproduction of the bug reported by the user: during a
/// crossing transition, `held_timeline_frame` (`frame_provider.rs`)
/// asks, for the "lent" side, for a stretch of source frames that
/// belongs to the NORMAL range of one clip but falls outside the range
/// declared by the OTHER — no normal `WantedRange` covers it, and
/// without `crossing_borrowed_segments` `SharedFrameCache::reconcile`
/// evicts it (or never fetches it) as soon as the playhead passes the cut,
/// freezing the compositing for half of the crossing window.
/// `clip_a` is trimmed short (30 of the 50 real frames available) and
/// `clip_b` starts at `source_in=5`: the crossing eats both the footage
/// discarded by the trim of `clip_a` and the one before the declared
/// start of `clip_b`.
#[test]
fn render_ahead_keeps_both_sides_of_a_crossing_readable_through_the_whole_window() {
    let (_dir_path_a, path_a) = make_test_clip("vv-app-render-ahead-test", "crossing_a.mp4", 2);
    let (_dir_path_b, path_b) = make_test_clip("vv-app-render-ahead-test", "crossing_b.mp4", 2);

    let mut project = Project::default();
    let item = |path: std::path::PathBuf| MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 50,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    };
    let media_a = project.media_pool.insert(item(path_a));
    let media_b = project.media_pool.insert(item(path_b));

    let clip_a = media_clip(1, media_a, 0, 30); // timeline [0,30)
    let clip_b = media_clip_trimmed(2, media_b, 30, 5, 30); // timeline [30,60), source_in=5

    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![clip_a, clip_b],
        muted: false,
        solo: false,
        locked: false,
        crossings: vec![vv_core::CrossTransition {
            left_clip: ClipId(1),
            right_clip: ClipId(2),
            transition: vv_core::Transition {
                kind: vv_core::TransitionKind::Push,
                duration: 16,
                direction: vv_core::PushDirection::Right,
                ease: vv_core::Ease::None,
                curve: 0.0,
            },
        }],
        mix: Default::default(),
        armed: Default::default(),
    }]));
    let timeline = project.timelines.get(timeline_id).unwrap().clone();

    let mut render_ahead = RenderAhead::spawn(
        project.clone(),
        timeline_id,
        100_000_000,
        None,
        DEFAULT_LOOKAHEAD_SECS,
        DEFAULT_BEHIND_SECS,
    );

    // Crossing window (duration=16, split 8/8 around the
    // cut at 30): [22,38). It covers a good margin before and after.
    let mut missing = Vec::new();
    for frame in 15..45 {
        render_ahead.set_target(frame);
        let start = std::time::Instant::now();
        let (mut ok, mut expected);
        loop {
            let clips = timeline.active_video_clips_at(frame);
            ok = 0;
            expected = 0;
            for (track_index, clip) in &clips {
                let involved = match timeline.tracks[*track_index].crossing_at(frame) {
                    Some((left, right, _)) if left.id == clip.id || right.id == clip.id => 2,
                    _ => 1,
                };
                expected += involved;
                let layers = crate::frame_provider::track_layers_at(
                    &project,
                    &timeline,
                    *track_index,
                    clip,
                    frame,
                    (320, 240),
                    &mut render_ahead,
                )
                .unwrap();
                ok += layers.len();
            }
            if ok >= expected || start.elapsed() > Duration::from_secs(5) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if ok < expected {
            missing.push((frame, ok, expected));
        }
    }
    assert!(
        missing.is_empty(),
        "frames with missing layers (frame, ok, expected): {missing:?}"
    );
}

/// End-to-end reproduction (real worker thread, not a direct
/// `walk_and_fill`) of the original scenario reported by the user: cut
/// a clip, place the playhead *still* just before the cut
/// point (lookahead window including a piece of both
/// halves, two segments of the same media). With the playhead really
/// still for several real poll cycles (not just two direct
/// calls to `walk_and_fill` as in the equivalent unit test), the
/// buffer must converge and stay stable — not recompute itself nor
/// shrink repeatedly.
#[test]
fn render_ahead_does_not_loop_when_the_playhead_sits_still_just_before_a_cut() {
    let (_dir_path, path) = make_test_clip_with_short_gop(
        "vv-app-render-ahead-test",
        "stationary_before_cut.mp4",
        20,
        25,
    );
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 500,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    // Cut at timeline_start=50 between two *contiguous* pieces of the
    // same file (a plain split, not a trim with a hole in between):
    // source [0,50) and then [50,150).
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![
            media_clip_trimmed(1, media_a, 0, 0, 50),
            media_clip_trimmed(2, media_a, 50, 50, 100),
        ],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let render_ahead = RenderAhead::spawn(
        project,
        timeline_id,
        100_000_000,
        None,
        DEFAULT_LOOKAHEAD_SECS,
        DEFAULT_BEHIND_SECS,
    );
    render_ahead.set_target(40);

    // Waits for the buffer to reach at least the cut.
    let start = std::time::Instant::now();
    loop {
        let ranges = render_ahead.cached_ranges_for(media_a);
        if ranges.iter().any(|&(s, e)| s <= 40 && e >= 50) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout: the buffer never reached the cut: ranges={ranges:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Playhead still for a handful of real poll cycles (50ms
    // each): if there were a compute/invalidate loop, the buffered
    // interval around the playhead would disappear and reappear
    // repeatedly instead of simply staying stable (or
    // growing forward, never shrinking from behind the playhead).
    let mut samples = Vec::new();
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(50));
        samples.push(render_ahead.cached_ranges_for(media_a));
    }
    for (i, ranges) in samples.iter().enumerate() {
        assert!(
            ranges.iter().any(|&(s, e)| s <= 40 && e >= 50),
            "sample {i}: the buffer around the playhead vanished with the playhead still: {ranges:?}"
        );
    }
}

/// plans/REFACTOR_PIPELINE.md §3.3: a freshly opened `OpenDecoder` has no
/// observations yet, so it uses the default fallback.
#[test]
fn open_decoder_seek_threshold_uses_the_default_fallback_before_any_observation() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "gop_fresh.mp4", 2);
    let decoder = Decoder::open(&path).unwrap();
    let od = OpenDecoder::fresh(decoder, path.clone(), false);
    assert_eq!(od.seek_threshold_frames(), DEFAULT_SEEK_THRESHOLD_FRAMES);
}

/// Two consecutive seek landings update the GOP estimate
/// to the distance observed between them.
#[test]
fn open_decoder_records_the_observed_gap_between_two_consecutive_landings() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "gop_observed.mp4", 2);
    let decoder = Decoder::open(&path).unwrap();
    let mut od = OpenDecoder::fresh(decoder, path.clone(), false);

    od.record_keyframe_landing(25, 25);
    od.record_keyframe_landing(50, 50);

    assert_eq!(od.seek_threshold_frames(), 25);
}

/// The estimate is a *minimum*: an accidental jump of several GOPs at
/// once (here a distance of 200 after one of 25) must not make
/// the threshold rise — only a *tighter* observation
/// narrows it further, never the opposite.
#[test]
fn open_decoder_gop_estimate_never_grows_from_a_wider_observation() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "gop_min.mp4", 2);
    let decoder = Decoder::open(&path).unwrap();
    let mut od = OpenDecoder::fresh(decoder, path.clone(), false);

    od.record_keyframe_landing(0, 0);
    od.record_keyframe_landing(25, 25); // distance 25: estimate = 25
    assert_eq!(od.seek_threshold_frames(), 25);

    od.record_keyframe_landing(225, 225); // distance 200: must not rise to 200
    assert_eq!(
        od.seek_threshold_frames(),
        25,
        "a jump wider than one already observed must not grow the estimate"
    );

    od.record_keyframe_landing(235, 235); // distance 10: must narrow
    assert_eq!(od.seek_threshold_frames(), 10);
}

/// A seek landing short of its target proves there is no keyframe in
/// between: below that distance an interrupted transit would seek back to
/// the same keyframe and start over, as at 2x on a 250-frame GOP.
#[test]
fn open_decoder_threshold_covers_the_stretch_a_seek_had_to_cross() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "gop_crossed.mp4", 2);
    let decoder = Decoder::open(&path).unwrap();
    let mut od = OpenDecoder::fresh(decoder, path.clone(), false);

    od.record_keyframe_landing(3750, 3922);
    assert_eq!(od.seek_threshold_frames(), 172);
    od.record_keyframe_landing(4000, 4010);
    assert_eq!(
        od.seek_threshold_frames(),
        250,
        "the distance between landings, wider than either crossing"
    );
    od.record_keyframe_landing(4000, 4290);
    assert_eq!(od.seek_threshold_frames(), 290);
}

/// Without the cap (`MAX_SEEK_THRESHOLD_FRAMES`), the *first*
/// observation alone could blow up the threshold if two
/// seeks happen to land many GOPs apart before a tighter
/// one arrives.
#[test]
fn open_decoder_gop_estimate_is_capped_even_on_the_first_observation() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "gop_cap.mp4", 2);
    let decoder = Decoder::open(&path).unwrap();
    let mut od = OpenDecoder::fresh(decoder, path.clone(), false);

    od.record_keyframe_landing(0, 0);
    od.record_keyframe_landing(10_000, 10_000);

    assert_eq!(od.seek_threshold_frames(), MAX_SEEK_THRESHOLD_FRAMES);
}

/// Regression: a fast and monotonic scrub (target always further
/// ahead, never a close landing) on a proxy must not leave
/// `seek_threshold_frames` stuck on an estimate as wide as
/// for the real source (`PROXY_SEEK_THRESHOLD_FRAMES` bypasses the
/// estimate entirely, see its docs) — this is exactly the scenario
/// diagnosed with `VV_DEBUG_RENDER_AHEAD=1`: without the bypass, every
/// worker cycle stayed stuck 15-60ms decoding in
/// sequence instead of seeking (almost free on an all-intra proxy),
/// more than the time between two ticks of a fast scrub.
#[test]
fn open_decoder_ignores_the_learned_gop_estimate_for_an_all_intra_proxy() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "gop_proxy_bypass.mp4", 2);
    let decoder = Decoder::open(&path).unwrap();
    let mut od = OpenDecoder::fresh(decoder, path.clone(), true);
    assert_eq!(od.seek_threshold_frames(), PROXY_SEEK_THRESHOLD_FRAMES);

    // Wide landings, never close together, as during a fast and
    // monotonic scrub: for a "normal" decoder the estimate
    // would converge on a large value (the minimum observed so far,
    // here 90) instead of narrowing towards the real GOP.
    od.record_keyframe_landing(90, 90);
    od.record_keyframe_landing(180, 180);
    od.record_keyframe_landing(270, 270);

    assert_eq!(
        od.seek_threshold_frames(),
        PROXY_SEEK_THRESHOLD_FRAMES,
        "an all-intra proxy must never use the learned threshold, whatever landing it observes"
    );
}

/// Regression: a real seek for an already open media must reuse
/// the existing decoder (`seek_to_time`), not throw it away to
/// reopen the file from scratch — for a large file not optimized for
/// streaming, reopening means reparsing the whole index every
/// time (seconds, even), and if that cost exceeds the tolerance the
/// target moves further during the opening itself, triggering
/// another one on the next round: a loop that never recovers
/// (observed: one frame every few seconds). Verified by the return
/// value: `Seeked` (reuse) instead of `Opened` (reopen) on the
/// second call on the same path.
///
/// Note: here the path is the same on both calls on
/// purpose — a *different* path for the same media_id now forces
/// a reopen even at the same position (see
/// `position_decoder_reopens_when_the_resolved_path_changes_even_without_a_seek`,
/// the proxy becoming available mid-session needs
/// exactly this).
#[test]
fn position_decoder_reuses_the_open_decoder_for_a_real_seek_instead_of_reopening_the_file() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "reuse.mp4", 3);
    let (media_a, _) = two_media_ids();

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    assert_eq!(
        position_decoder(
            &caches,
            &mut open,
            media_a,
            &path,
            0,
            false,
            false,
            false,
            HwPriority::Normal,
            None,
        ),
        Positioned::Opened
    );

    assert_eq!(
        position_decoder(
            &caches,
            &mut open,
            media_a,
            &path,
            1000,
            false,
            false,
            false,
            HwPriority::Normal,
            None,
        ),
        Positioned::Seeked,
        "a real seek on an already open media must reuse the decoder, not reopen it"
    );
}

/// plans/REFACTOR_PIPELINE.md proxy: a proxy becoming available in the
/// background (or the "use proxy" toggle changing) makes a
/// different path resolve for the same media_id — the decoder open on the
/// old path makes no sense to reuse/seek (it points at a
/// different file), it must be reopened from scratch even if the requested position
/// would otherwise be "close enough" not to justify a
/// seek.
#[test]
fn position_decoder_reopens_when_the_resolved_path_changes_even_without_a_seek() {
    let (_dir_path_a, path_a) = make_test_clip("vv-app-render-ahead-test", "swap_a.mp4", 2);
    let (_dir_path_b, path_b) = make_test_clip("vv-app-render-ahead-test", "swap_b.mp4", 2);
    let (media_a, _) = two_media_ids();

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    assert_eq!(
        position_decoder(
            &caches,
            &mut open,
            media_a,
            &path_a,
            0,
            false,
            false,
            false,
            HwPriority::Normal,
            None,
        ),
        Positioned::Opened
    );

    // Same requested position (0): without the check on the resolved
    // path, `needs_seek` would be `false` (0 is not "too far ahead"
    // relative to a freshly opened decoder) and the call
    // would return `Reused` — reusing a decoder pointing at the
    // wrong file.
    assert_eq!(
        position_decoder(
            &caches,
            &mut open,
            media_a,
            &path_b,
            0,
            false,
            false,
            false,
            HwPriority::Normal,
            None,
        ),
        Positioned::Opened,
        "the path changed: it must reopen on the new one, not reuse the old decoder"
    );
}

/// Regression for a real bug, confirmed by the user: `next_frame`
/// is only what the decoder *believes* it has already produced, not a
/// guarantee that it is still cached — an eviction between one cycle
/// and the next (`reconcile`, or a tight budget during an earlier
/// fill) may have removed the tail the decoder thinks it
/// already has behind it. Here exactly this is simulated: a
/// decoder "advanced" with `next_frame` past the target, but with the
/// content next_frame assumes it covered removed by hand
/// from the cache (as a real `reconcile` would) — `position_decoder`
/// must notice and force a real seek, not trust
/// `next_frame` and return `Reused` over a hole.
#[test]
fn position_decoder_reseeks_when_next_frame_claims_coverage_the_cache_no_longer_has() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "stale_next_frame.mp4", 3);
    let (media_a, _) = two_media_ids();

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    assert_eq!(
        position_decoder(
            &caches,
            &mut open,
            media_a,
            &path,
            0,
            false,
            false,
            false,
            HwPriority::Normal,
            None,
        ),
        Positioned::Opened
    );
    // Decodes and caches a few frames, as
    // walk_and_fill would — the decoder now "believes" it is ahead with
    // that whole stretch genuinely cached behind it.
    for _ in 0..20 {
        let od = open.get_mut(&media_a).unwrap();
        match od.decoder.next_frame() {
            Ok(Some((idx, frame))) => {
                caches.insert(media_a, idx, frame);
                od.next_frame = idx + 1;
            }
            _ => break,
        }
    }
    let advanced_next_frame = open.get(&media_a).unwrap().next_frame;
    assert!(
        advanced_next_frame > 5,
        "the decoder must have advanced a lot"
    );

    // Simulates a real eviction: `reconcile` with a window that
    // deliberately excludes a single frame in the middle of what
    // `next_frame` assumes covered — the cache loses that frame without
    // the decoder knowing anything about it (exactly what
    // a tight budget or a shrinking window would do).
    let gap_at = advanced_next_frame - 3;
    let window = [
        WantedRange {
            media_id: media_a,
            source_start: 0,
            source_end: gap_at - 1,
            timeline_start: 0,
            rate: vv_core::Rational::one(),
        },
        WantedRange {
            media_id: media_a,
            source_start: gap_at + 1,
            source_end: advanced_next_frame - 1,
            timeline_start: gap_at + 1,
            rate: vv_core::Rational::one(),
        },
    ];
    caches.reconcile(0, &window, usize::MAX);

    assert_eq!(
        position_decoder(
            &caches,
            &mut open,
            media_a,
            &path,
            0,
            false,
            false,
            false,
            HwPriority::Normal,
            None,
        ),
        Positioned::Seeked,
        "a hole left by an eviction behind next_frame must force a real seek, \
             not a Reused that leaves it uncovered forever"
    );
}

/// Regression for the reported bug: during normal playback the
/// decoder is almost always *ahead* of the target (it is the healthy
/// state of a buffer working well). Before the fix,
/// `position_decoder` read this as "too far behind" and
/// reopened the file with a real seek on every poll cycle,
/// invalidating the work just done — hence the indicator
/// "going in circles" without ever advancing steadily.
#[test]
fn position_decoder_does_not_reseek_when_already_usefully_ahead_of_the_segment_start() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "steady.mp4", 3);
    let (media_a, _) = two_media_ids();

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    assert_eq!(
        position_decoder(
            &caches,
            &mut open,
            media_a,
            &path,
            0,
            false,
            false,
            false,
            HwPriority::Normal,
            None,
        ),
        Positioned::Opened
    );

    // Decodes a few frames forward "by hand" *and* inserts them into
    // the cache, as walk_and_fill would, to simulate a decoder already
    // buffered past the current target — since the coverage recheck
    // in `position_decoder` (see its docs), an
    // advanced `next_frame` without cached content behind it
    // would no longer be enough to avoid a seek.
    for _ in 0..20 {
        let od = open.get_mut(&media_a).unwrap();
        match od.decoder.next_frame() {
            Ok(Some((idx, frame))) => {
                caches.insert(media_a, idx, frame);
                od.next_frame = idx + 1;
            }
            _ => break,
        }
    }
    let advanced_next_frame = open.get(&media_a).unwrap().next_frame;
    assert!(advanced_next_frame > 0, "the decoder must have advanced");

    // A later cycle with the target still behind the decoder's
    // position — the normal state during forward playback —
    // must not reopen/reset the decoder.
    assert_eq!(
        position_decoder(
            &caches,
            &mut open,
            media_a,
            &path,
            0,
            false,
            false,
            false,
            HwPriority::Normal,
            None,
        ),
        Positioned::Reused
    );
    assert_eq!(
        open.get(&media_a).unwrap().next_frame,
        advanced_next_frame,
        "it must not have reopened the decoder while still usefully ahead"
    );
}

/// `WalkOutcome::caught_up` is the basis of `RenderAhead::is_caught_up`,
/// which the UI uses to decide whether it is worth requesting another
/// repaint (see the docs there): with a budget ample enough for
/// the whole lookahead window, one cycle must be enough to cover it
/// all and signal so.
#[test]
fn walk_and_fill_reports_caught_up_when_the_whole_window_fits_the_budget() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "caught_up.mp4", 3);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 75,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 75)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let generous_budget = 320 * 240 * 4 * 200; // well past the 75 frames of the window
    let outcome = walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        0,
        generous_budget,
        false,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(0),
    );

    assert!(
        outcome.caught_up,
        "budget and window cover the whole clip: nothing else should be left to do"
    );
    assert!(!outcome.interrupted);
}

/// The retention window behind the playhead (`behind_secs`) is not
/// just "do not discard what is already there": on a zone *never visited
/// before* it must really be decoded, not only retained if
/// already present — otherwise a scrub in a new zone shortly after
/// the start of the clip would have nothing to retain behind it.
#[test]
fn walk_and_fill_decodes_the_behind_window_on_a_fresh_area() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "fresh_behind.mp4", 4);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 100,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 100)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    // Generous budget: no capacity eviction to confuse the
    // result, here only "does it get decoded" or not matters.
    let generous_budget = 320 * 240 * 3 / 2 * 200;

    // First time this zone is seen: playhead at 60, never
    // anywhere else before (`went_backward` irrelevant on the first
    // cycle).
    walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        60,
        generous_budget,
        false,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(60),
    );

    let ranges = caches.cached_ranges(media_a);
    assert!(
        ranges.iter().any(|&(s, e)| s <= 40 && e >= 59),
        "the span behind the playhead [40,59] (within behind_secs) must have been decoded, not just kept if already there: {ranges:?}"
    );
    assert!(
        ranges.iter().any(|&(s, e)| s <= 60 && e >= 99),
        "the forward window must still be covered normally: {ranges:?}"
    );
}

/// Regression reported by the user: during a backwards scrub
/// the frame useful *right away* is the one adjacent to the playhead (the near
/// edge of the window behind), not the one on the far edge — but
/// decoding a whole segment behind in a single seek (as for
/// the forward window) produces the frames in the wrong order:
/// ffmpeg decodes only forwards from `source_start` (far) towards
/// `source_end` (near), so the most useful frame arrives
/// last. With a budget covering the forward window (small,
/// near the end of the clip) plus only the first chunk of the
/// window behind (`BEHIND_CHUNK_FRAMES`), the frame adjacent to the
/// playhead must still be cached, the one on the far edge
/// not — direct proof that `chunk_behind_segments_near_to_far`
/// really reorders the decoding priority, not just on paper.
#[test]
fn walk_and_fill_decodes_the_behind_window_nearest_frames_first_under_a_tight_budget() {
    // GOP=1 (every frame a keyframe, like a proxy): a seek lands
    // exactly where requested, so the budget needed for each
    // chunk is predictable in exact frames — with a long GOP (a
    // "normal" video) the seek would land on the keyframe nearest
    // *before* the target, making the computation below fragile
    // without adding anything to what is under test (the priority
    // order of the chunks, not how much it costs to get there).
    let (_dir_path, path) =
        make_test_clip_with_short_gop("vv-app-render-ahead-test", "behind_priority.mp4", 4, 1);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 100,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 100)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    // Playhead at 95: tiny forward window (only [95,99], the
    // clip ends at 100), normal window behind (2s = 50 frames,
    // [45,94]). Budget for the forward window (5 frames) plus *only*
    // the first chunk of the window behind (`BEHIND_CHUNK_FRAMES`
    // = 15 frames, [80,94]) — not enough to reach the far
    // edge at 45.
    let frame_bytes = 320 * 240 * 3 / 2;
    let tight_budget = frame_bytes * (5 + 15);
    walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        95,
        tight_budget,
        false,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(95),
    );

    assert!(
        caches.contains(media_a, 94),
        "the frame next to the playhead (near edge of the behind window) must be \
             among the first decoded, so cached even with a tight budget"
    );
    assert!(
        !caches.contains(media_a, 45),
        "the frame on the far edge of the behind window must not be reached before \
             those near the playhead, with a budget covering only the first chunk"
    );
}

/// Regression: with the playhead *still* (no change between two cycles), a
/// second `walk_and_fill` on an already entirely filled window behind
/// must not touch the decoder — see the docs of
/// `without_already_cached_chunks`. Without the filter, the order from the
/// nearest chunk to the farthest means the decoder, at the start of a
/// cycle, is always positioned *behind* the first requested chunk
/// (it had stopped where the farthest chunk of the previous cycle
/// ended), so every chunk would be reseeked and at least one frame
/// thrown away again — on every single cycle, forever, even without
/// any scrub going on. A real seek involves the
/// `ffmpeg` process/the container: even one costs orders of magnitude
/// more than a round of in-memory `cached_ranges` checks, so a
/// tight time cap on the second round reliably distinguishes
/// "it did not touch the decoder" from "it redid work".
#[test]
fn walk_and_fill_does_not_reseek_an_already_complete_behind_window_when_idle() {
    let (_dir_path, path) = make_test_clip_with_short_gop(
        "vv-app-render-ahead-test",
        "behind_idle_stability.mp4",
        4,
        1,
    );
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 100,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 100)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let generous_budget = 320 * 240 * 3 / 2 * 200;

    // First round: fills entirely both ahead and behind (several
    // chunks, [45,94] at 25fps/2s).
    let first = walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        95,
        generous_budget,
        false,
        None,
        1,
        DEFAULT_LOOKAHEAD_SECS,
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(95),
    );
    assert!(
        first.caught_up,
        "the first round must complete the window: {first:?}"
    );

    // 20 later rounds, same playhead, nothing changes: they must
    // complete almost instantly (no real seek, only
    // in-memory `cached_ranges` checks). A single round is not
    // a stable enough measure (OS scheduling jitter
    // of a few ms can happen even without any real
    // work) — summing 20 independent rounds amplifies the
    // signal: if even one touches the decoder, the cost of a
    // real seek+decode (1-2ms, already measured elsewhere in this
    // file) dominates the total, while 20 rounds of in-memory checks
    // only stay in the hundreds of µs.
    let start = std::time::Instant::now();
    for _ in 0..20 {
        let outcome = walk_and_fill(
            &project,
            timeline_id,
            &caches,
            &mut open,
            &mut open_behind,
            95,
            generous_budget,
            false,
            None,
            1,
            DEFAULT_LOOKAHEAD_SECS,
            DEFAULT_BEHIND_SECS,
            &LiveTarget::still(95),
        );
        assert!(outcome.caught_up);
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(20),
        "20 rounds with the playhead still must not touch the decoder (no real seek): \
             took {elapsed:?} in total, expected <20ms"
    );
}

/// `lookahead_secs`/`behind_secs` configured to `0`: the window
/// shrinks to the minimum margin (`MIN_MARGIN_FRAMES`), not to zero — even
/// with a generous budget and a zone never seen before (which with a
/// normal window would trigger both the forward window and
/// the retention one, see the test above).
#[test]
fn walk_and_fill_buffers_only_a_minimal_margin_when_configured_to_zero_seconds() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "no_read_ahead.mp4", 4);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 100,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 100)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let generous_budget = 320 * 240 * 3 / 2 * 200;

    let outcome = walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        60,
        generous_budget,
        false,
        None, // proxy: irrelevant for this test
        1,
        0.0, // lookahead_secs: the one under test
        0.0, // behind_secs: the one under test
        &LiveTarget::still(60),
    );

    // [56,63], not a wider interval: with the default keyint
    // (250) and a clip of only 100 frames, the only keyframe is at 0 —
    // reaching frame 63 (= 60 + MIN_MARGIN_FRAMES) requires
    // decoding in sequence from there, and those *transit* frames
    // stay cached as a by-product DURING the round (see the docs
    // of `transit_bytes` in `fill_segments`: it serves to make the
    // chunk behind [56,59] find them already there instead of
    // crossing the same GOP from scratch again), but a final `reconcile`
    // discards them again before `walk_and_fill` declares
    // `caught_up` (see the comment there on why: otherwise the UI,
    // which stops requesting repaints on `caught_up`, can stay
    // stuck showing that transit as if it were "buffered" until
    // the next repaint for other reasons — bug reported
    // by the user, the strip "resizes itself" only by moving the
    // mouse). The reuse *between* the two segments of this same round has
    // already happened before this final `reconcile`, only
    // its survival past the end of the round is lost.
    assert_eq!(
        caches.cached_ranges(media_a),
        vec![(60 - MIN_MARGIN_FRAMES, 60 + MIN_MARGIN_FRAMES - 1)],
        "set to zero seconds the window must not extend past the minimum margin"
    );
    assert!(
        outcome.caught_up,
        "a minimal window, already covered, must report caught_up"
    );
}

/// Regression: if the budget is not enough to cover the whole lookahead
/// window, the buffer must still start from the playhead (the
/// nearest frames, the most useful to show right away) and not from an
/// arbitrary tail of the window — otherwise the indicator shows
/// an interval that "falls after" the playhead without ever covering it.
#[test]
fn walk_and_fill_prioritizes_frames_near_the_playhead_when_the_budget_is_too_small_for_the_full_window()
 {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "small_budget.mp4", 3);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 75,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 75)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    // Tiny budget: the lookahead window (3s = 75 frames at
    // 25fps) does not fit entirely in the cache.
    let tiny_budget = 320 * 240 * 4 * 5;
    walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        0,
        tiny_budget,
        false,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(0),
    );

    let ranges = caches.cached_ranges(media_a);
    assert!(!ranges.is_empty());
    assert_eq!(
        ranges[0].0, 0,
        "the buffer must start from the playhead, not from an arbitrary tail: {ranges:?}"
    );
    assert!(
        ranges[0].1 < 74,
        "with such a small budget it must not manage to cover the whole window: {ranges:?}"
    );
}

/// plans/REFACTOR_PIPELINE.md §3.1 (responsiveness): if the *live* target has
/// already moved past the fallback threshold (no GOP observation
/// made for this media yet) relative to `from_frame`
/// before even starting, the fill must notice at the first
/// opportunity (after the first decoded frame) and stop
/// returning `true`, instead of continuing to decode for the whole
/// window a prefetch that is by now obsolete.
#[test]
fn walk_and_fill_stops_early_and_reports_true_when_the_live_target_has_already_drifted() {
    let (_dir_path, path) =
        make_test_clip_with_short_gop("vv-app-render-ahead-test", "reactivity_drift.mp4", 20, 25);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 500,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 500)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    // The live target is already past the threshold relative to from_frame=0 before
    // the fill even starts: it simulates the playhead having jumped
    // elsewhere while this cycle was about to start.
    let drifted_target = LiveTarget::still(DEFAULT_SEEK_THRESHOLD_FRAMES + 200);

    let outcome = walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        0,
        100_000_000,
        false,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &drifted_target,
    );
    assert!(
        outcome.interrupted,
        "it must report the interruption to the caller (worker_loop) so it restarts right away"
    );
    assert!(
        !outcome.caught_up,
        "interrupted: it could not check whether the window was covered"
    );

    let ranges = caches.cached_ranges(media_a);
    let decoded_frames: FrameIdx = ranges.iter().map(|&(s, e)| e - s + 1).sum();
    assert!(
        decoded_frames < 10,
        "it must stop after very few frames, not decode the whole now-stale window: ranges={ranges:?}"
    );
}

/// Regression for the bug reported by the user: two clips on the
/// timeline sharing the same media (a single file cut
/// into several pieces, very common) generate two `WantedRange`s for the
/// same `media_id` in the same window, with different `source_start`s.
/// Calling `evict_before` with the `source_start` of the
/// *single* segment being processed (as was done before)
/// discarded, while processing the second segment, everything the first
/// had just decoded — "the buffer recomputes itself from scratch
/// invalidating the following frames" reported by the user,
/// reproducible at every cut between two pieces of the same file.
#[test]
fn walk_and_fill_does_not_invalidate_one_segment_while_processing_another_segment_of_the_same_media()
 {
    let (_dir_path, path) =
        make_test_clip_with_short_gop("vv-app-render-ahead-test", "same_media_cut.mp4", 20, 25);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 500,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    // Cut at timeline_start=60 between two pieces of the same file:
    // the first uses source [0,60), the second restarts from a
    // point much further into the source [200,300) — exactly
    // like cutting away a middle part of the same file.
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![
            media_clip_trimmed(1, media_a, 0, 0, 60),
            media_clip_trimmed(2, media_a, 60, 200, 100),
        ],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let budget = 100_000_000;

    // The lookahead window (3s = 75 frames at 25fps) from 40
    // crosses the cut at 60, including a piece of both
    // clips in the same cycle.
    walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        40,
        budget,
        false,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(40),
    );

    let ranges = caches.cached_ranges(media_a);
    assert!(
        ranges.iter().any(|&(s, e)| s <= 40 && e >= 59),
        "the span of the first clip [40,59] must not be evicted by processing the second: ranges={ranges:?}"
    );
    assert!(
        ranges.iter().any(|&(s, e)| s <= 200 && e >= 200),
        "the second clip must still be buffered, not just crossed: ranges={ranges:?}"
    );
}

/// Regression for the bug reported by the user ("the caching must not
/// be per clip but per timeline"): the previous test uses a
/// huge budget (100MB) that never puts the real capacity of the
/// cache under pressure, so it does not catch it. With a tight
/// budget, two segments of the same media in this window (a
/// cut with a discarded part in between: sources far from
/// each other) asked *together* for more frames than the shared
/// `FrameCache` could hold — the second segment processed
/// (`[200,254]`) evicted by capacity limit (ordinary LRU,
/// not `evict_before`) everything the first (`[40,59]`) had
/// just decoded in the very same cycle, even though
/// `evict_before` alone would have protected it. As seen by the user:
/// every clip seems to buffer "on its own", at the expense of the
/// others — hence "the caching looks per-clip, not per-timeline".
#[test]
fn walk_and_fill_does_not_let_one_segment_of_a_media_evict_another_via_capacity_when_the_budget_is_tight()
 {
    let (_dir_path, path) = make_test_clip_with_short_gop(
        "vv-app-render-ahead-test",
        "same_media_cut_tight_budget.mp4",
        20,
        25,
    );
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 500,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    // Same cut as the previous test: [0,60) then [200,300).
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![
            media_clip_trimmed(1, media_a, 0, 0, 60),
            media_clip_trimmed(2, media_a, 60, 200, 100),
        ],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    // Capacity ~60 YUV420 frames (width*height*3/2 bytes/frame):
    // less than what the two segments together would ask for (~20 + ~55),
    // but more than what each asks for alone — it forces the
    // sharing of the same cache to really count.
    let budget = 60 * 320 * 240 * 3 / 2;

    let outcome = walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        40,
        budget,
        false,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(40),
    );

    let ranges = caches.cached_ranges(media_a);
    assert!(
        ranges.iter().any(|&(s, e)| s <= 40 && e >= 59),
        "the span of the first clip [40,59] must not vanish because of the second segment in the same cycle: ranges={ranges:?}"
    );
    assert!(
        ranges.iter().any(|&(s, _)| s <= 200),
        "the second clip must still get a slice of the shared capacity: ranges={ranges:?}"
    );
    assert!(
        !outcome.caught_up,
        "budget full before finishing the window: not \"caught up\", there is still work for the next cycle"
    );
}

/// Regression for the bug reported by the user: with the playhead
/// still just before a cut (it is enough to cut a clip and
/// place the playhead just before the cut point: the
/// lookahead window includes a piece of both
/// halves anyway, two segments of the same media), every poll cycle
/// reprocesses the same two segments in the same order: the second
/// segment must not make the first look like it "went backwards" and
/// trigger a real seek on every cycle while standing still (see the docs
/// of `position_decoder` on `went_backward`). Verified by passing
/// `false` (playhead still) to both segments in both cycles.
///
/// Gap between the two segments (10 and 25) chosen on purpose below
/// `DEFAULT_SEEK_THRESHOLD_FRAMES`: here `position_decoder` is
/// called directly, without ever decoding a real frame, so
/// no GOP observation ever happens and the threshold stays at the
/// fallback for the whole test — a wider gap would legitimately trigger
/// the "too far ahead" branch, masking the thing
/// this test wants to isolate (the contamination between segments of the
/// same media, not that threshold).
#[test]
fn position_decoder_does_not_reseek_across_cycles_when_the_same_media_appears_in_two_segments() {
    let (_dir_path, path) =
        make_test_clip("vv-app-render-ahead-test", "same_media_two_segments.mp4", 3);
    let (media_a, _) = two_media_ids();
    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();

    // Cycle 1: two segments of the same media in the same window
    // (as on the two sides of a cut), source_start 10 and then 25.
    assert_eq!(
        position_decoder(
            &caches,
            &mut open,
            media_a,
            &path,
            10,
            false,
            false,
            false,
            HwPriority::Normal,
            None,
        ),
        Positioned::Opened
    );
    assert_eq!(
        position_decoder(
            &caches,
            &mut open,
            media_a,
            &path,
            25,
            false,
            false,
            false,
            HwPriority::Normal,
            None,
        ),
        Positioned::Reused,
        "in the same cycle the second segment must never need a seek: the decoder is already there"
    );

    // Cycle 2, playhead still (`went_backward=false` for both):
    // the same two segments. Reprocessing the *first* segment (10) must
    // not look like it "went backwards" just because the last call
    // seen in the previous cycle was for the following segment (25).
    assert_eq!(
        position_decoder(
            &caches,
            &mut open,
            media_a,
            &path,
            10,
            false,
            false,
            false,
            HwPriority::Normal,
            None,
        ),
        Positioned::Reused,
        "playhead still: reprocessing the first segment must not trigger a real seek"
    );
    assert_eq!(
        position_decoder(
            &caches,
            &mut open,
            media_a,
            &path,
            25,
            false,
            false,
            false,
            HwPriority::Normal,
            None,
        ),
        Positioned::Reused
    );
}

/// Regression (ex-A2, plans/REFACTOR_PIPELINE.md): with the old
/// architecture (one `FrameCache` per media, capacity fixed at
/// creation time) this test checked that the capacity
/// of `media_b` widened when `media_a` left the window
/// — a problem that with the globally budgeted `SharedFrameCache` (§2)
/// can no longer arise *by construction*: there is no longer a
/// per-media capacity to keep in sync, the budget is a single one
/// and always the real one. So it checks the direct equivalent: with
/// fewer media contending for the budget, `media_b` gets to
/// buffer *more* frames (no longer capacity, but real coverage).
#[test]
fn walk_and_fill_buffers_more_of_a_media_once_fewer_distinct_media_share_the_budget() {
    let (_dir_path_a, path_a) = make_test_clip("vv-app-render-ahead-test", "resize_a.mp4", 2);
    let (_dir_path_b, path_b) = make_test_clip("vv-app-render-ahead-test", "resize_b.mp4", 15);

    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path: path_a,
        meta: MediaMeta {
            duration_frames: 40,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let media_b = project.media_pool.insert(MediaItem {
        path: path_b,
        meta: MediaMeta {
            duration_frames: 375,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![
            media_clip(1, media_a, 0, 40),   // [0,40)
            media_clip(2, media_b, 40, 400), // [40,440)
        ],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    // A budget that at 320x240 (307_200 B/frame) is enough for ~60 frames
    // in total: with two media contending for the window few fit
    // each, with only one many more.
    let total_budget = 60 * 320 * 240 * 4;

    let frames_cached = |ranges: &[(FrameIdx, FrameIdx)]| -> FrameIdx {
        ranges.iter().map(|&(s, e)| e - s + 1).sum()
    };

    // First cycle: the lookahead window (3s = 75 frames) crosses
    // the cut at 40, so media_a and media_b are both in the
    // window and share the same global budget.
    walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        0,
        total_budget,
        false,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(0),
    );
    let frames_b_shared = frames_cached(&caches.cached_ranges(media_b));

    // Second cycle: the target is well past the cut, only media_b is
    // in the window — reconcile discards media_a (Tier A), so
    // media_b has the whole global budget to itself, without needing
    // any separate capacity to "resize upwards".
    walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        200,
        total_budget,
        false,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(200),
    );
    let ranges = caches.cached_ranges(media_b);
    let frames_b_alone = frames_cached(&ranges);
    assert!(
        frames_b_alone > frames_b_shared,
        "with a single media in the window it must buffer more of it, not stay at the share it had when sharing: {frames_b_shared} -> {frames_b_alone}"
    );

    // source_start for the second cycle: clip b has source_in=0,
    // timeline_start=40, hence 200-40=160.
    assert!(
        ranges.iter().any(|&(s, _)| s <= 160),
        "with more budget available the buffer must be able to start from the new playhead, not from an arbitrary tail further ahead: {ranges:?}"
    );
}

/// Regression: after a scrub far back relative to where the
/// worker had already buffered forward, the buffer must
/// reach the new position too — the decoder can only
/// decode forwards, so without an explicit reopen
/// it would stay stuck past the new target forever.
#[test]
fn render_ahead_catches_up_after_a_large_backward_seek() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "backward_seek.mp4", 4);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 100,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 100)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let render_ahead = RenderAhead::spawn(
        project,
        timeline_id,
        100_000_000,
        None,
        DEFAULT_LOOKAHEAD_SECS,
        DEFAULT_BEHIND_SECS,
    );
    render_ahead.set_target(80);

    let start = std::time::Instant::now();
    loop {
        if !render_ahead.cached_ranges_for(media_a).is_empty() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout in avanti"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Scrub back past the seek threshold: the decoder that
    // buffered around 80 cannot continue forwards to
    // reach 0.
    render_ahead.set_target(0);
    let start = std::time::Instant::now();
    loop {
        let ranges = render_ahead.cached_ranges_for(media_a);
        if ranges.iter().any(|&(s, _)| s == 0) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout backwards: ranges={ranges:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Regression for the bug reported by the user: a backwards
/// scrub must regenerate the buffer for the new position,
/// not stay stuck on the nearest cached frame. The scrub here
/// (70 frames) is chosen on purpose *past* the retention window
/// behind the playhead (`DEFAULT_BEHIND_SECS`, 2s = 50 frames at 25fps): a
/// real scrub past that window must still behave as
/// before that window existed — regenerate from scratch — because here there
/// is nothing to reuse. A scrub *inside* the window, on the other hand, must
/// not regenerate anything by construction (see
/// `walk_and_fill_does_not_redecode_the_already_buffered_tail_after_a_small_backward_seek`,
/// which checks exactly that). Before the original fix of
/// this regression, it stayed stuck on the nearest cached
/// frame because `position_decoder` considered "far enough ahead"
/// any target still behind `next_frame` by more than a threshold —
/// but `walk_and_fill` discards on every cycle everything outside
/// the current window, so even a small step back
/// past the retention window falls into already discarded territory and
/// is unreachable by decoding only forwards.
#[test]
fn render_ahead_catches_up_after_a_backward_seek_beyond_the_retention_window() {
    let (_dir_path, path) =
        make_test_clip("vv-app-render-ahead-test", "small_backward_seek.mp4", 6);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 150,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 150)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let render_ahead = RenderAhead::spawn(
        project,
        timeline_id,
        100_000_000,
        None,
        DEFAULT_LOOKAHEAD_SECS,
        DEFAULT_BEHIND_SECS,
    );
    render_ahead.set_target(100);

    // Wait not only for the buffer to cover 100, but for the later poll
    // cycles to have also already discarded what is left
    // outside the window (forward+behind) of 100 — including 30, which
    // is 70 frames behind, past the 50 of the retention window.
    // Otherwise the test would pass by accident, because the first
    // fill (which decodes from the nearest keyframe, here
    // the start of the file) already includes 30 before it is even
    // discarded.
    let covers = |ranges: &[(FrameIdx, FrameIdx)], f: FrameIdx| {
        ranges.iter().any(|&(s, e)| s <= f && f <= e)
    };
    let start = std::time::Instant::now();
    loop {
        let ranges = render_ahead.cached_ranges_for(media_a);
        if covers(&ranges, 100) && !covers(&ranges, 30) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout in avanti: ranges={ranges:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Scrub back by 70 frames (past the retention window of
    // 50): it must still regenerate the buffer for the new
    // position, not stay stuck on the nearest frame already
    // cached. Coverage of 30 (not "a range starting exactly
    // there"): the seek lands on the keyframe nearest to 30, which may
    // be even before 30 itself.
    render_ahead.set_target(30);
    let start = std::time::Instant::now();
    loop {
        let ranges = render_ahead.cached_ranges_for(media_a);
        if covers(&ranges, 30) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout on backward scrub: ranges={ranges:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Regression for the bug reported by the user: stuttering playback
/// even at 1x with `lookahead_secs`/`behind_secs` at `0`, because
/// `set_target` updated only an atomic read by the worker at most every
/// `POLL_INTERVAL` (50ms) — a real cap at ~20 frames/sec
/// regardless of how fast the decoding was (it happened
/// with the proxies too).
///
/// It checks the immediate wake-up alone (`Command::Wake`), isolated from the
/// minimum margin: one isolated jump at a time, with a deadline
/// of 40ms *after* waiting for the worker to settle on the
/// previous target. Without the immediate wake-up the wait would be
/// uniform between 0 and 50ms: each of the 11 jumps stays under 40ms by
/// chance 80% of the time, all together ~9%. The deadline is no tighter
/// because under the load of the other tests in parallel even
/// decoding the frame can overrun.
#[test]
fn render_ahead_reacts_to_each_target_change_faster_than_the_old_poll_interval() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "wake_on_change.mp4", 2);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 50,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 50)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let render_ahead = RenderAhead::spawn(project, timeline_id, 100_000_000, None, 0.0, 0.0);

    let wait_for = |frame: FrameIdx| {
        let start = std::time::Instant::now();
        loop {
            if render_ahead.get_frame(media_a, frame).is_some() {
                return start.elapsed();
            }
            assert!(
                start.elapsed() < Duration::from_millis(40),
                "frame {frame} not ready within a deadline compatible with the immediate wakeup"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    };

    // First target: no tight deadline, the worker only has to
    // start up (opening the decoder included).
    render_ahead.set_target(0);
    loop {
        if render_ahead.get_frame(media_a, 0).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    // From here on, every isolated jump has 40ms to be ready.
    for target in (8..=48).step_by(4) {
        render_ahead.set_target(target);
        wait_for(target);
    }
}

/// General regression: after some forward playback over several
/// cycles, a backwards scrub towards a position more recent than the
/// very first target ever seen must still make the buffer
/// recompute for the new position. It checks that `went_backward`,
/// computed once per cycle (simulated here as
/// `worker_loop` would), holds over a realistic sequence of cycles, not just a
/// single isolated backwards jump.
#[test]
fn walk_and_fill_catches_up_after_a_backward_seek_above_the_historical_minimum() {
    let (_dir_path, path) = make_test_clip_with_short_gop(
        "vv-app-render-ahead-test",
        "backward_above_historical_min.mp4",
        20,
        25,
    );
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 500,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 500)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    // Tight budget: the whole window does not fit in the cache, so
    // as it advances `evict_before` really discards the frames behind
    // the playhead instead of just leaving them still present by
    // chance.
    let budget = 43_000_000;

    // Forward playback over several cycles: `went_backward` is always
    // `false` (every target is >= the previous one), exactly as
    // `worker_loop` would compute it comparing `from` with the previous
    // cycle.
    let mut prev = None;
    for from in [0, 50, 100, 150, 200] {
        let went_backward = prev.is_some_and(|p| from < p);
        prev = Some(from);
        walk_and_fill(
            &project,
            timeline_id,
            &caches,
            &mut open,
            &mut open_behind,
            from,
            budget,
            went_backward,
            None, // proxy: irrelevant for this test
            1,
            DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
            DEFAULT_BEHIND_SECS,
            &LiveTarget::still(from),
        );
    }

    // At this point the frames around 0 are certainly evicted
    // (evict_before discarded everything behind the playhead
    // on every cycle, the last of which is 200).
    let ranges_before = caches.cached_ranges(media_a);
    assert!(
        !ranges_before.iter().any(|&(s, e)| s <= 80 && e >= 80),
        "80 must not already be cached by coincidence, otherwise the test proves nothing: {ranges_before:?}"
    );

    // Scrub back to 80: further back than the current playhead (200),
    // but further ahead than the oldest target ever seen (0).
    walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        80,
        budget,
        true,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(80),
    );

    let ranges_after = caches.cached_ranges(media_a);
    assert!(
        ranges_after.iter().any(|&(s, e)| s <= 80 && e >= 80),
        "scrubbing back to 80 must recompute the buffer for the new position: {ranges_after:?}"
    );
}

/// Checks the requested optimization: after a small backwards
/// scrub, the already buffered portion still falling within
/// the *new* window (forward + behind, see `behind_secs`) must
/// not be re-decoded — indeed, the decoder must not be touched at
/// all (`SharedFrameCache::covers`, checked *before* consulting it:
/// see its docs on a real bug, confirmed in production, caused
/// by trusting the position the decoder *believes* it has instead
/// of asking the cache). With the retention window behind the
/// playhead, the 30-frame scrub below falls entirely
/// *inside* that window (50 frames): the stretch [250,269] is not
/// even discarded by `reconcile`, so the whole requested segment
/// comes out already covered and is skipped outright — `OpenDecoder::
/// next_frame` must stay exactly where it was before this
/// call, direct proof that no seek/decode happened.
///
/// Note (plans/REFACTOR_PIPELINE.md §2, Tier A): with the globally budgeted
/// `SharedFrameCache`, `reconcile` also discards what is *past*
/// the horizon of the new window (here: past 344, given that the
/// new playhead is 270) — unlike the old `evict_before`,
/// which discarded only what was behind and left intact everything
/// that was ahead, whatever the horizon. This is intended: the budget
/// of the window is always exactly that of the current
/// window, not an indefinite accumulation of historical tails. So here
/// it only checks that [250,344] (the intersection between the old tail and
/// the new window widened by the retention) is reachable without
/// re-decoding it — not that the whole old tail up to 374
/// survives.
#[test]
fn walk_and_fill_does_not_redecode_the_already_buffered_tail_after_a_small_backward_seek() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "reconnect.mp4", 20);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 500,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 500)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let budget = 43_000_000; // capacity ~139 frames

    // Buffers around 300: with keyint=250 (libx264 default) the
    // decoder restarts from keyframe 250 and fills up to the capacity
    // limit.
    walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        300,
        budget,
        false,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(300),
    );
    let filled_up_to = open.get(&media_a).unwrap().next_frame - 1;
    assert!(
        filled_up_to > 350,
        "the first fill must have buffered well past 300 (up to the lookahead horizon): {filled_up_to}"
    );

    // Scrub back by only 30 frames: below the old threshold of
    // 120, but still a real backwards move (it must
    // reopen/reseek, `went_backward=true`).
    walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        270,
        budget,
        true,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(270),
    );

    let next_frame_after = open.get(&media_a).unwrap().next_frame;
    assert_eq!(
        next_frame_after,
        filled_up_to + 1,
        "the requested segment was already fully cached: the decoder should not have been \
             touched at all, next_frame must stay where it was: next_frame={next_frame_after}"
    );

    // The intersection between the old tail and the new window
    // widened by the retention ([250,344]) must be reachable
    // as a contiguous range, without holes due to a wasted
    // re-decode. The fact that `filled_up_to` (374) is further ahead
    // than the horizon of the new window is expected: that part was
    // discarded by Tier A of `reconcile` because it is no longer in the
    // current window (see the note above), not because the
    // reconnection failed.
    let ranges = caches.cached_ranges(media_a);
    assert!(
        ranges.iter().any(|&(s, e)| s <= 250 && e >= 344),
        "the intersection [250,344] between old tail and new window must stay one contiguous range: ranges={ranges:?} filled_up_to={filled_up_to}"
    );
}

/// Regression for the infinite loop reported by the user and
/// diagnosed with `VV_DEBUG_RENDER_AHEAD` on a real file
/// (1080p60fps, long GOP, proxy off): a small backwards
/// scrub reconnects the decoder early (as in the test
/// above) leaving `next_frame` parked *before* `source_start` —
/// far enough back to exceed the adaptive threshold. Before the
/// fix, every later cycle with the playhead *still* at the same
/// position still saw `segment_start > next_frame + threshold`
/// (the decoder position is never updated by a cycle
/// that skips it), so it reseeked, re-decoded the same stretch
/// already cached until reconnecting at the same point as before — a
/// stable and infinite loop, never self-limiting. `SharedFrameCache::covers`
/// prevents it by asking the cache *before* looking at the
/// (presumed) position of the decoder.
#[test]
fn walk_and_fill_does_not_loop_forever_after_reconnecting_early_from_a_backward_seek() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "reconnect_loop.mp4", 20);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 500,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 500)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let budget = 43_000_000; // capacity ~139 frames, as above

    // Same setup as the test above: initial fill at 300, then
    // a small scrub back to 290 that reconnects early.
    walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        300,
        budget,
        false,
        None,
        1,
        DEFAULT_LOOKAHEAD_SECS,
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(300),
    );
    walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        290,
        budget,
        true,
        None,
        1,
        DEFAULT_LOOKAHEAD_SECS,
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(290),
    );

    // 20 later cycles, playhead still at 290 (no scrub): in the
    // original bug each one reseeked and re-decoded from scratch,
    // dominating the total time (real seek+decode, not just
    // in-memory checks) — same threshold/logic as the
    // stability-at-rest test above.
    let start = std::time::Instant::now();
    for _ in 0..20 {
        walk_and_fill(
            &project,
            timeline_id,
            &caches,
            &mut open,
            &mut open_behind,
            290,
            budget,
            false,
            None,
            1,
            DEFAULT_LOOKAHEAD_SECS,
            DEFAULT_BEHIND_SECS,
            &LiveTarget::still(290),
        );
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(20),
        "20 cycles with the playhead still after an early reattach must not re-seek/\
             re-decode repeatedly: took {elapsed:?} in total, expected <20ms"
    );
}

/// Regression for a real bug, confirmed by the user with a
/// diagnostic log on a 1080p60fps file: the reconnection check
/// inside `fill_segments` checked two points (`next_frame` and
/// `segment.source_end`) with two *independent* `contains` — if
/// both happen by chance to fall in two separate cache islands (a
/// hole between them, left by an earlier fill saturated on budget),
/// the check passed anyway, making it believe the segment
/// was already covered when in reality there was a hole right in the
/// middle, never reached before nor after — permanent, because the
/// decoder stopped there convinced it had finished.
#[test]
fn fill_segments_bridges_the_gap_between_two_disconnected_cached_islands() {
    // Explicit GOP=10: keyframes at 0,10,20,... — it only needs to make
    // one predictable near the start of the requested segment, no
    // other requirement on the distance between the two islands below.
    let (_dir_path, path) =
        make_test_clip_with_short_gop("vv-app-render-ahead-test", "bridge_gap.mp4", 4, 10);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 100,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    // `fill_segments` does not need a timeline: it already works at
    // the level of a resolved segment.

    let caches = SharedFrameCache::new();
    // Two disconnected islands, a real hole in between ([16,39], never
    // touched by anything so far) — as an earlier fill saturated
    // on budget would leave.
    for idx in 5..=15 {
        caches.insert(media_a, idx, Arc::new(dummy_frame()));
    }
    for idx in 40..=50 {
        caches.insert(media_a, idx, Arc::new(dummy_frame()));
    }

    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let segment = WantedRange {
        media_id: media_a,
        source_start: 5,
        source_end: 50,
        timeline_start: 5,
        rate: Rational::one(),
    };
    let pacing = RefCell::new(Pacing::default());
    let ctx = FillContext {
        project: &project,
        caches: &caches,
        went_backward: false,
        cache_budget_bytes: usize::MAX,
        from_frame: 5,
        proxy: None,
        preview_divisor: 1,
        target: &LiveTarget::still(5),
        jumps: 0,
        window: &[],
        hw_priority: HwPriority::Normal,
        keyframe_first: false,
        pacing: &pacing,
        decide_by: None,
    };
    let _ = fill_segments(std::slice::from_ref(&segment), &ctx, &mut open);

    let ranges = caches.cached_ranges(media_a);
    assert!(
        ranges.iter().any(|&(s, e)| s <= 5 && e >= 50),
        "the hole [16,39] between the two islands must be filled, not left uncovered \
             forever by a premature reattach: ranges={ranges:?}"
    );
}

/// Regression for the same real bug confirmed by the user: even
/// after fixing the premature reconnection above, a tight budget
/// could still prevent filling a hole far from the
/// nearest keyframe — because the pure transit frames (decoded
/// only to cross a long GOP towards the requested segment,
/// never part of any wanted window) were inserted into the cache and
/// counted against the budget like everything else, and could saturate it
/// before even reaching the stretch actually requested. Here a
/// budget that is enough for the requested segment but not for all
/// the transit preceding it too must still manage to fill it.
#[test]
fn fill_segments_does_not_let_transit_frames_exhaust_the_budget_before_the_wanted_range() {
    let (_dir_path, path) = make_test_clip_with_short_gop(
        "vv-app-render-ahead-test",
        "transit_budget.mp4",
        4,
        250, // long GOP: no keyframe between 0 and the requested segment
    );
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 100,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    // A budget that is only enough for the requested segment (80,90], 11
    // frames, not for the 80 frames of pure transit to
    // decode to reach it from the keyframe at 0.
    let frame_bytes = 320 * 240 * 3 / 2;
    let tight_budget = frame_bytes * 11;
    let segment = WantedRange {
        media_id: media_a,
        source_start: 80,
        source_end: 90,
        timeline_start: 80,
        rate: Rational::one(),
    };
    let pacing = RefCell::new(Pacing::default());
    let ctx = FillContext {
        project: &project,
        caches: &caches,
        went_backward: false,
        cache_budget_bytes: tight_budget,
        from_frame: 80,
        proxy: None,
        preview_divisor: 1,
        target: &LiveTarget::still(80),
        jumps: 0,
        window: &[],
        hw_priority: HwPriority::Normal,
        keyframe_first: false,
        pacing: &pacing,
        decide_by: None,
    };
    let _ = fill_segments(std::slice::from_ref(&segment), &ctx, &mut open);

    let ranges = caches.cached_ranges(media_a);
    assert!(
        ranges.iter().any(|&(s, e)| s <= 80 && e >= 90),
        "the requested segment [80,90] must be reached, not left uncovered because \
             the budget ran out on the transit before getting there: ranges={ranges:?}"
    );
}

/// Regression for a real bug, confirmed by the user with a
/// diagnostic log during normal playback on a real 1080p60fps file
/// with GOP=250: a first attempt at this check
/// estimated the necessary transit from the observed GOP and skipped the
/// segment if that estimate alone exceeded the budget — violating
/// the very rule the budget check on the *wanted* frame
/// respects on purpose (the transit never counts against the budget of
/// its own stretch). The real effect: the *forward* segment (not
/// only those behind) stayed stuck for hundreds of consecutive
/// frames every time the transit estimate looked
/// large, even with plenty of free budget — playback stalling
/// for seconds every time the playhead crossed a GOP
/// boundary. Here it checks that an "experienced" decoder (which already knows
/// the GOP and the last keyframe, hence would estimate a huge transit for
/// a far segment) does NOT prevent filling a
/// segment near the playhead when there is plenty of budget — only
/// the availability of room for the wanted stretch counts, never an
/// estimate of how much transit it takes to get there.
#[test]
fn fill_segments_does_not_block_a_reachable_segment_just_because_its_transit_would_be_large() {
    let (_dir_path, path) = make_test_clip_with_short_gop(
        "vv-app-render-ahead-test",
        "transit_estimate_does_not_block.mp4",
        4,
        25,
    );
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path: path.clone(),
        meta: MediaMeta {
            duration_frames: 100,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    // "Experienced" decoder: it already knows GOP=25 and a keyframe at 0. The
    // segment below (24,34) requires a *real* transit of 24
    // frames (almost a whole GOP) to be reached from the nearest
    // keyframe — genuine, not the result of a stale anchor: if
    // the check estimated it and compared it with the tight budget
    // below (which is enough for the wanted stretch anyway, 11 frames),
    // it would block the segment despite it being actually
    // reachable.
    let frame_bytes = 320 * 240 * 3 / 2;
    let mut od = OpenDecoder::fresh(Decoder::open(&path).unwrap(), path, false);
    od.record_keyframe_landing(0, 0);
    od.record_keyframe_landing(25, 25);
    open.insert(media_a, od);

    let segment = WantedRange {
        media_id: media_a,
        source_start: 24,
        source_end: 34,
        timeline_start: 24,
        rate: Rational::one(),
    };
    // Enough for the wanted stretch (11 frames) with plenty of margin, but
    // less than the estimated transit (24 frames): with budget=frame_bytes*20
    // the old check (estimated transit >= free space, even
    // if that space is not needed by the transit at all) blocked it
    // anyway.
    let budget_enough_for_the_wanted_range_but_not_the_full_transit = frame_bytes * 20;
    let pacing = RefCell::new(Pacing::default());
    let ctx = FillContext {
        project: &project,
        caches: &caches,
        went_backward: true,
        cache_budget_bytes: budget_enough_for_the_wanted_range_but_not_the_full_transit,
        from_frame: 24,
        proxy: None,
        preview_divisor: 1,
        target: &LiveTarget::still(24),
        jumps: 0,
        window: &[],
        hw_priority: HwPriority::Normal,
        keyframe_first: false,
        pacing: &pacing,
        decide_by: None,
    };
    let outcome = fill_segments(std::slice::from_ref(&segment), &ctx, &mut open);

    assert_eq!(
        outcome,
        ControlFlow::Continue(()),
        "the wanted span has enough budget: it must not be skipped just because the \
             transit to reach it is estimated large"
    );
    assert!(
        caches
            .cached_ranges(media_a)
            .iter()
            .any(|&(s, e)| s <= 24 && e >= 34),
        "segment [24,34] must be cached"
    );
}

/// Regression for the bug reported by the user and confirmed by the real
/// diagnostic log: when the playhead advances in small steps (never
/// enough to exceed the seek threshold and force a real
/// seek) the decoder stays comfortably ahead and continues from where it
/// was — correct and intended (see `position_decoder`) — but the
/// cache was evicted by the standard LRU alone, which removes the
/// oldest only when new frames *arrive*, not when the *playhead
/// moves*: the front of the buffer therefore stayed stuck far
/// behind the playhead for an indefinite time, while the
/// tail grew by a few frames on every cycle — exactly the
/// fixed gap "the buffer always starts a few frames after the
/// playhead" reported by the user (confirmed with a tight budget
/// forcing the capacity to be exceeded on every cycle). With the
/// retention window behind the playhead, the buffer also covers a
/// stretch *before* each target: the right check now is that the
/// playhead is covered (no longer in a gap), not that a range starts
/// exactly there.
#[test]
fn walk_and_fill_keeps_the_buffer_front_at_the_playhead_even_without_a_real_reseek() {
    let (_dir_path, path) =
        make_test_clip("vv-app-render-ahead-test", "front_tracks_target.mp4", 20);

    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 500,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media_a, 0, 500)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));

    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    // Tight budget: every advance of 10 frames adds more
    // frames than the cache can hold without evicting some,
    // forcing the eviction to act on every cycle.
    let budget = 43_000_000;

    walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        10,
        budget,
        false,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(10),
    );

    let mut target = 300;
    walk_and_fill(
        &project,
        timeline_id,
        &caches,
        &mut open,
        &mut open_behind,
        target,
        budget,
        false,
        None, // proxy: irrelevant for this test
        1,
        DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
        DEFAULT_BEHIND_SECS,
        &LiveTarget::still(target),
    );
    for _ in 0..15 {
        target += 10;
        walk_and_fill(
            &project,
            timeline_id,
            &caches,
            &mut open,
            &mut open_behind,
            target,
            budget,
            false,
            None, // proxy: irrelevant for this test
            1,
            DEFAULT_LOOKAHEAD_SECS, // read_ahead: irrelevant for this test
            DEFAULT_BEHIND_SECS,
            &LiveTarget::still(target),
        );
        let ranges = caches.cached_ranges(media_a);
        assert!(
            ranges.iter().any(|&(s, e)| s <= target && target <= e),
            "the buffer must cover the playhead (target={target}): {ranges:?}"
        );
    }
}

/// The transit from the keyframe to a segment is decoded but not cached
/// unless another wanted range (here the window behind) covers it.
#[test]
fn fill_segments_caches_only_the_transit_some_window_wants() {
    let (_dir_path, path) = make_test_clip_with_short_gop(
        "vv-app-render-ahead-test",
        "transit_outside_window.mp4",
        4,
        250,
    );
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 100,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let range = |source_start, source_end| WantedRange {
        media_id: media_a,
        source_start,
        source_end,
        timeline_start: source_start,
        rate: Rational::one(),
    };
    let segment = range(80, 90);
    let window = [range(60, 79), segment];
    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let pacing = RefCell::new(Pacing::default());
    let ctx = FillContext {
        project: &project,
        caches: &caches,
        went_backward: false,
        cache_budget_bytes: usize::MAX,
        from_frame: 80,
        proxy: None,
        preview_divisor: 1,
        target: &LiveTarget::still(80),
        jumps: 0,
        window: &window,
        hw_priority: HwPriority::Normal,
        keyframe_first: false,
        pacing: &pacing,
        decide_by: None,
    };
    let _ = fill_segments(std::slice::from_ref(&segment), &ctx, &mut open);

    assert_eq!(caches.cached_ranges(media_a), vec![(60, 90)]);
    assert_eq!(open[&media_a].last_keyframe_landed, Some(0));
}

/// A jump of the user abandons the cycle however short it is: the frames
/// it was heading to belong to a position no longer wanted.
#[test]
fn fill_segments_is_interrupted_by_a_jump_below_the_seek_threshold() {
    let (_dir_path, path) =
        make_test_clip_with_short_gop("vv-app-render-ahead-test", "jump_interrupts.mp4", 4, 25);
    let mut project = Project::default();
    let media_a = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 100,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let segment = WantedRange {
        media_id: media_a,
        source_start: 10,
        source_end: 60,
        timeline_start: 10,
        rate: Rational::one(),
    };
    let target = LiveTarget::still(12);
    target.jumps.fetch_add(1, Ordering::Relaxed);
    let caches = SharedFrameCache::new();
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    let pacing = RefCell::new(Pacing::default());
    let ctx = FillContext {
        project: &project,
        caches: &caches,
        went_backward: false,
        cache_budget_bytes: usize::MAX,
        from_frame: 10,
        proxy: None,
        preview_divisor: 1,
        target: &target,
        jumps: 0,
        window: std::slice::from_ref(&segment),
        hw_priority: HwPriority::Normal,
        keyframe_first: false,
        pacing: &pacing,
        decide_by: None,
    };

    let flow = fill_segments(std::slice::from_ref(&segment), &ctx, &mut open);
    assert!(matches!(flow, ControlFlow::Break(outcome) if outcome.interrupted));
    assert!(!caches.covers(media_a, 10, 60));
}

fn make_color_clip(
    dir_name: &str,
    file_name: &str,
    color: &str,
) -> (vv_media::test_support::TempDir, std::path::PathBuf) {
    let dir = vv_media::test_support::TempDir::new(dir_name);
    let path = dir.join(file_name);
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("color=c={color}:size=320x240:rate=25:duration=2"),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );
    (dir, path)
}

fn single_media_project(path: std::path::PathBuf) -> (Project, MediaId, TimelineId) {
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        content_hash: vv_media::content_fingerprint(&path).unwrap(),
        path,
        meta: MediaMeta {
            duration_frames: 50,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        compound: None,
        folder: None,
    });
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media, 0, 50)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]));
    (project, media, timeline_id)
}

fn wait_caught_up(render_ahead: &RenderAhead) {
    let start = std::time::Instant::now();
    // Past `POLL_INTERVAL`: `caught_up` is set only by a whole cycle.
    std::thread::sleep(POLL_INTERVAL * 3);
    while !render_ahead.is_caught_up() {
        assert!(start.elapsed() < Duration::from_secs(10), "never caught up");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The first media of any project gets the same `MediaId`. Opening another project must not show the old one's frames.
#[test]
fn a_replaced_project_never_shows_the_previous_projects_frames() {
    let dir = "vv-app-render-ahead-replace-test";
    let (_dir_path_a, path_a) = make_test_clip(dir, "a.mp4", 2);
    let (_dir_path_b, path_b) = make_color_clip(dir, "b.mp4", "red");
    let (project_a, media_a, timeline_a) = single_media_project(path_a);
    let (project_b, media_b, timeline_b) = single_media_project(path_b.clone());
    assert_eq!(
        media_a, media_b,
        "the premise: both projects reuse the same key"
    );

    let render_ahead = RenderAhead::spawn(project_a, timeline_a, 100_000_000, None, 1.0, 0.0);
    render_ahead.set_target(0);
    wait_caught_up(&render_ahead);
    assert!(render_ahead.get_frame(media_a, 0).is_some());

    render_ahead.update_project(&project_b, timeline_b);
    wait_caught_up(&render_ahead);
    let shown = render_ahead
        .get_frame(media_b, 0)
        .expect("frame 0 of the new project");
    let (_, expected) = vv_media::Decoder::open(&path_b)
        .unwrap()
        .next_frame()
        .unwrap()
        .unwrap();
    assert!(
        shown.y == expected.y,
        "frame 0 still comes from the previous project's file"
    );
}

#[test]
fn a_preview_divisor_caches_smaller_frames_and_a_change_drops_the_old_ones() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-divisor-test", "divisor.mp4", 1);
    let (project, media, timeline_id) = single_media_project(path);
    let render_ahead = RenderAhead::spawn(project, timeline_id, 100_000_000, None, 1.0, 0.0);
    render_ahead.set_preview_divisor(2);
    render_ahead.set_target(0);
    wait_caught_up(&render_ahead);
    let frame = render_ahead.get_frame(media, 0).expect("frame 0");
    assert_eq!((frame.width, frame.height), (160, 120));

    render_ahead.set_preview_divisor(1);
    wait_caught_up(&render_ahead);
    let frame = render_ahead.get_frame(media, 0).expect("frame 0");
    assert_eq!((frame.width, frame.height), (320, 240));
}

/// The UI sets the window on every frame and repaints until caught up: an
/// unchanged window must not undo it, or an idle editor redraws forever.
#[test]
fn an_unchanged_window_keeps_render_ahead_caught_up() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-idle-test", "idle.mp4", 1);
    let (project, _, timeline_id) = single_media_project(path);
    let render_ahead = RenderAhead::spawn(project, timeline_id, 100_000_000, None, 1.0, 0.5);
    render_ahead.set_target(0);
    wait_caught_up(&render_ahead);

    render_ahead.set_lookahead_secs(1.0);
    render_ahead.set_behind_secs(0.5);
    render_ahead.set_target(0);
    assert!(render_ahead.is_caught_up());
}

/// An open decoder for `media` on `path`, and a cached frame of it.
fn opened_media(
    media: MediaId,
    path: &std::path::Path,
) -> (SharedFrameCache, HashMap<MediaId, OpenDecoder>) {
    let caches = SharedFrameCache::new();
    let mut open = HashMap::new();
    assert_eq!(
        position_decoder(
            &caches,
            &mut open,
            media,
            path,
            0,
            false,
            false,
            false,
            HwPriority::Normal,
            None,
        ),
        Positioned::Opened
    );
    caches.insert(media, 0, Arc::new(dummy_frame()));
    (caches, open)
}

#[test]
fn an_edit_that_keeps_the_media_keeps_its_decoder_and_frames() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-forget-test", "keep.mp4", 1);
    let (old, media, timeline_id) = single_media_project(path.clone());
    let mut new = old.clone();
    new.timelines[timeline_id].tracks[0].clips[0]
        .effects
        .transform = vv_core::TransformTracks::constant(vv_core::Transform {
        zoom: [2.0, 2.0],
        ..Default::default()
    });
    let (caches, mut open) = opened_media(media, &path);
    let mut open_behind = HashMap::new();

    forget_changed_media(&old, &new, &caches, &mut open, &mut open_behind);
    assert!(open.contains_key(&media));
    assert!(caches.contains(media, 0));
}

#[test]
fn a_media_naming_another_file_loses_its_decoder_and_frames() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-forget-test", "before.mp4", 1);
    let (old, media, _) = single_media_project(path.clone());
    let mut relinked = old.clone();
    relinked.media_pool[media].path = "elsewhere.mp4".into();
    let mut removed = old.clone();
    removed.media_pool.remove(media);

    for new in [relinked, removed] {
        let (caches, mut open) = opened_media(media, &path);
        let mut open_behind = HashMap::new();
        forget_changed_media(&old, &new, &caches, &mut open, &mut open_behind);
        assert!(!open.contains_key(&media));
        assert!(!caches.contains(media, 0));
        assert_eq!(caches.bytes_used(), 0);
    }
}

/// Black frames after repositioning during 1x playback, on a real file:
/// `VV_BENCH_CLIP=<path> cargo test --release -p vv-app bench_black_frames -- --ignored --nocapture`,
/// with `VV_BENCH_HW=auto|nvdec|vulkan` to decode on the GPU.
/// Keyframes are assumed every 250 frames (a 60 fps OBS recording).
#[test]
#[ignore = "manual measurement, not a correctness assertion"]
fn bench_black_frames_after_repositioning_during_playback() {
    let Ok(path) = std::env::var("VV_BENCH_CLIP") else {
        return;
    };
    if let Ok(mode) = std::env::var("VV_BENCH_HW") {
        crate::hw_decode::apply(crate::hw_decode::HwDecodeMode::from_id(&mode).unwrap());
        eprintln!("HW decode: {:?}", crate::hw_decode::devices());
    }
    let path = std::path::PathBuf::from(path);
    let meta = vv_media::probe(&path).unwrap();
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        content_hash: vv_media::content_fingerprint(&path).unwrap(),
        path,
        meta: meta.clone(),
        compound: None,
        folder: None,
    });
    let mut timeline = timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media, 0, meta.duration_frames)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);
    timeline.fps = meta.fps;
    let fps = meta.fps.as_f64();
    let timeline_id = project.timelines.insert(timeline);
    // Default budget and windows shrunk to fit it, as `effective_window_secs` does.
    let budget = crate::DEFAULT_CACHE_BUDGET_BYTES;
    let frame_bytes = vv_media::yuv420_frame_bytes(meta.width, meta.height) as f64;
    let affordable_secs = budget as f64 * 0.85 / (frame_bytes * fps);
    let scale = (affordable_secs / (DEFAULT_LOOKAHEAD_SECS + DEFAULT_BEHIND_SECS)).min(1.0);
    let render_ahead = RenderAhead::spawn(
        project,
        timeline_id,
        budget,
        None,
        DEFAULT_LOOKAHEAD_SECS * scale,
        DEFAULT_BEHIND_SECS * scale,
    );

    let gop = 250;
    // The first two teach the worker the GOP, as the first clicks in the app do.
    let clicks = [
        (20, 220),
        (22, 125),
        (24, 30),
        (26, 125),
        (28, 220),
        (30, 60),
        (32, 180),
    ];
    let (last_gop, last_offset) = clicks[clicks.len() - 1];
    let needed = last_gop * gop + last_offset + (3.0 * fps) as FrameIdx;
    assert!(
        meta.duration_frames >= needed,
        "the clip must last at least {needed} frames ({:.0} s): past its end every refresh is black",
        needed as f64 / fps
    );
    for (gop_index, offset) in clicks {
        let start = gop_index * gop + offset;
        let t0 = std::time::Instant::now();
        let mut first_shown = None;
        let mut black = 0;
        let mut frames = 0;
        while t0.elapsed() < Duration::from_secs(3) {
            let playhead = start + (t0.elapsed().as_secs_f64() * fps) as FrameIdx;
            if frames == 0 {
                render_ahead.jump_to(playhead);
            } else {
                render_ahead.set_target(playhead);
            }
            if render_ahead.get_frame(media, playhead).is_some() {
                first_shown.get_or_insert(t0.elapsed());
            } else {
                black += 1;
            }
            frames += 1;
            std::thread::sleep(Duration::from_secs_f64(1.0 / fps));
        }
        eprintln!(
            "click {offset:>3} frames past a keyframe: first frame after {:>6.0} ms, black {black}/{frames} refreshes in 3 s",
            first_shown.map_or(f64::NAN, |d| d.as_secs_f64() * 1000.0)
        );
    }
}

#[test]
#[ignore = "manual"]
fn bench_backward_scrub_with_proxy() {
    let Ok(path) = std::env::var("VV_BENCH_CLIP") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let meta = vv_media::probe(&path).unwrap();
    let hash = vv_media::content_fingerprint(&path).unwrap();
    let quality = vv_media::proxy::ProxyQuality::Medium;
    let proxy = vv_media::proxy::proxy_path_for(hash, quality);
    if !proxy.is_file() {
        vv_media::proxy::generate_proxy(&path, hash, quality, &[], |_| true).unwrap();
    }
    let pmeta = vv_media::probe(&proxy).unwrap();
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        content_hash: hash,
        path,
        meta: meta.clone(),
        compound: None,
        folder: None,
    });
    let mut timeline = timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media, 0, meta.duration_frames)],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);
    timeline.fps = meta.fps;
    let fps = meta.fps.as_f64();
    let timeline_id = project.timelines.insert(timeline);
    let budget = crate::DEFAULT_CACHE_BUDGET_BYTES;
    let frame_bytes = vv_media::yuv420_frame_bytes(pmeta.width, pmeta.height) as f64;
    let affordable_secs = budget as f64 * 0.85 / (frame_bytes * fps);
    let scale = (affordable_secs / (DEFAULT_LOOKAHEAD_SECS + DEFAULT_BEHIND_SECS)).min(1.0);
    let render_ahead = RenderAhead::spawn(
        project,
        timeline_id,
        budget,
        Some(quality),
        DEFAULT_LOOKAHEAD_SECS * scale,
        DEFAULT_BEHIND_SECS * scale,
    );
    let speed: f64 = std::env::var("VV_BENCH_SPEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0);
    for dir in [1.0f64, -1.0] {
        let start: FrameIdx = 20_000;
        render_ahead.jump_to(start);
        std::thread::sleep(Duration::from_secs(3));
        let t0 = std::time::Instant::now();
        let (mut hit, mut total, mut last) = (0, 0, start);
        let mut longest_miss = 0;
        let mut miss_run = 0;
        while t0.elapsed() < Duration::from_secs(4) {
            let p = start + (dir * speed * t0.elapsed().as_secs_f64() * fps) as FrameIdx;
            if p != last {
                render_ahead.jump_to(p);
                last = p;
            }
            if render_ahead.get_frame(media, p).is_some() {
                hit += 1;
                miss_run = 0;
            } else {
                miss_run += 1;
                longest_miss = longest_miss.max(miss_run);
            }
            total += 1;
            std::thread::sleep(Duration::from_millis(16));
        }
        eprintln!(
            "dir {dir:+} speed {speed}: shown {hit}/{total} refreshes, longest black run {longest_miss}"
        );
    }
}

#[test]
fn source_stride_follows_the_clip_rate_and_is_one_without_a_stride() {
    assert_eq!(source_stride(2, Rational::one()), 2);
    assert_eq!(source_stride(4, Rational::one()), 4);
    assert_eq!(
        source_stride(2, Rational::new(2, 1)),
        1,
        "25 fps media on 50 fps"
    );
    assert_eq!(source_stride(4, Rational::new(2, 1)), 2);
    assert_eq!(source_stride(2, Rational::new(1, 2)), 4, "a clip at 200%");
    assert_eq!(source_stride(1, Rational::new(1, 2)), 1);
    assert_eq!(source_stride(3, Rational::new(4, 5)), 3);
}

/// 75 frames at 25 fps, cut at frame 0 of the timeline.
fn project_with_one_clip(
    file_name: &str,
    clip: impl FnOnce(MediaId) -> Clip,
) -> (
    Project,
    TimelineId,
    MediaId,
    vv_media::test_support::TempDir,
) {
    let (dir, path) = make_test_clip("vv-app-render-ahead-test", file_name, 3);
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 75,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let clip = clip(media);
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![clip],
        ..Track::new(TrackKind::Video)
    }]));
    (project, timeline_id, media, dir)
}

fn fill_once(
    project: &Project,
    timeline_id: TimelineId,
    caches: &SharedFrameCache,
    from: FrameIdx,
    target: &LiveTarget,
) -> WalkOutcome {
    let mut open = HashMap::new();
    let mut open_behind = HashMap::new();
    walk_and_fill(
        project,
        timeline_id,
        caches,
        &mut open,
        &mut open_behind,
        from,
        320 * 240 * 4 * 200,
        false,
        None,
        1,
        DEFAULT_LOOKAHEAD_SECS,
        0.0,
        target,
    )
}

fn strided_target(frame: FrameIdx, stride: FrameIdx) -> LiveTarget {
    let target = LiveTarget::still(frame);
    target.frame_stride.store(stride, Ordering::Relaxed);
    target
}

#[test]
fn walk_and_fill_with_a_stride_caches_only_its_multiples() {
    let (project, timeline_id, media, _dir) =
        project_with_one_clip("stride_multiples.mp4", |m| media_clip(1, m, 0, 60));
    let caches = SharedFrameCache::new();

    let outcome = fill_once(&project, timeline_id, &caches, 0, &strided_target(0, 2));

    assert!(outcome.caught_up);
    assert_eq!(caches.stride(media), 2);
    assert!((0..60).step_by(2).all(|i| caches.contains(media, i)));
    let odd: Vec<FrameIdx> = (1..60)
        .step_by(2)
        .filter(|&i| caches.contains(media, i))
        .collect();
    assert!(
        odd.is_empty(),
        "skipped, not decoded into the cache: {odd:?}"
    );
}

/// Paused again after fast playback: every frame is wanted, the gaps are
/// filled and no neighbour stands in for the exact frame any more.
#[test]
fn walk_and_fill_fills_the_gaps_when_the_stride_goes_back_to_one() {
    let (project, timeline_id, media, _dir) =
        project_with_one_clip("stride_back.mp4", |m| media_clip(1, m, 0, 60));
    let caches = SharedFrameCache::new();
    fill_once(&project, timeline_id, &caches, 0, &strided_target(0, 2));

    let outcome = fill_once(&project, timeline_id, &caches, 31, &LiveTarget::still(31));

    assert!(outcome.caught_up);
    assert_eq!(caches.stride(media), 1);
    assert!((31..60).all(|i| caches.contains(media, i)));
    assert_eq!(
        caches.get_near(media, 33, 0, 59).map(|(_, how)| how),
        Some(vv_media::Served::Exact)
    );
}

/// A freeze frame is one source frame, here not a multiple: it must still be
/// decoded, or the layer would vanish during fast playback.
#[test]
fn walk_and_fill_with_a_stride_still_decodes_a_freeze_frame() {
    let (project, timeline_id, media, _dir) = project_with_one_clip("stride_freeze.mp4", |m| {
        let mut clip = media_clip(1, m, 0, 40);
        clip.freeze = Some(13);
        clip
    });
    let caches = SharedFrameCache::new();

    let outcome = fill_once(&project, timeline_id, &caches, 0, &strided_target(0, 4));

    assert!(outcome.caught_up);
    assert!(caches.contains(media, 13));
}

/// A clip at 25 fps on a 50 fps timeline shows each source frame twice: at
/// 2x every source frame is still shown, none may be skipped.
#[test]
fn walk_and_fill_keeps_every_frame_of_a_conformed_clip_at_2x() {
    let (mut project, timeline_id, media, _dir) =
        project_with_one_clip("stride_conformed.mp4", |m| {
            Clip::from_source_range(
                ClipId(1),
                ClipSource::Media(m),
                0,
                40,
                0,
                Rational::new(2, 1),
            )
        });
    project.timelines[timeline_id].fps = Rational::new(50, 1);
    let caches = SharedFrameCache::new();

    fill_once(&project, timeline_id, &caches, 0, &strided_target(0, 2));

    assert_eq!(caches.stride(media), 1);
    assert!((0..40).all(|i| caches.contains(media, i)));
}

/// Two clips of one media, one sped up: the media takes the smaller stride,
/// or the other clip would miss frames it shows.
#[test]
fn walk_and_fill_gives_a_media_the_smallest_stride_of_its_clips() {
    let (mut project, timeline_id, media, _dir) =
        project_with_one_clip("stride_two_rates.mp4", |m| {
            media_clip_trimmed(1, m, 0, 0, 20)
        });
    let fast = Clip::from_source_range(
        ClipId(2),
        ClipSource::Media(media),
        30,
        70,
        20,
        Rational::new(1, 2),
    );
    project.timelines[timeline_id].tracks[0].insert_sorted(fast);
    let caches = SharedFrameCache::new();

    fill_once(&project, timeline_id, &caches, 0, &strided_target(0, 2));

    assert_eq!(caches.stride(media), 2, "not the 4 of the clip at 200%");
}

/// 8 s at 25 fps with a keyframe every 25 frames, one clip over all of it.
fn project_with_gop_25(
    file_name: &str,
) -> (
    Project,
    TimelineId,
    MediaId,
    vv_media::test_support::TempDir,
) {
    let (dir, path) = make_test_clip_with_short_gop("vv-app-render-ahead-test", file_name, 8, 25);
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: 200,
            fps: Rational::new(25, 1),
            width: 320,
            height: 240,
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
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, media, 0, 200)],
        ..Track::new(TrackKind::Video)
    }]));
    (project, timeline_id, media, dir)
}

fn playing_target(frame: FrameIdx, speed: f64) -> LiveTarget {
    let target = LiveTarget::still(frame);
    store_secs(&target.speed, speed);
    target.keyframes_when_behind.store(true, Ordering::Relaxed);
    target
}

fn fill_paced(
    project: &Project,
    timeline_id: TimelineId,
    caches: &SharedFrameCache,
    open: &mut HashMap<MediaId, OpenDecoder>,
    from: FrameIdx,
    target: &LiveTarget,
    pacing: &RefCell<Pacing>,
) -> WalkOutcome {
    walk_and_fill_paced(
        project,
        timeline_id,
        caches,
        open,
        &mut HashMap::new(),
        &mut HashMap::new(),
        from,
        320 * 240 * 4 * 400,
        false,
        None,
        1,
        DEFAULT_LOOKAHEAD_SECS,
        0.0,
        target,
        pacing,
    )
}

fn decide(
    project: &Project,
    timeline_id: TimelineId,
    caches: &SharedFrameCache,
    speed: f64,
    pacing: &mut Pacing,
) -> bool {
    let timeline = &project.timelines[timeline_id];
    let segments = collect_media_segments(project, timeline, 0, 75);
    let gops = HashMap::from([(segments[0].media_id, 25)]);
    !keyframes_only_media(
        project, 25.0, &segments, caches, None, &gops, 0, speed, true, pacing,
    )
    .is_empty()
}

#[test]
fn keyframes_only_follows_the_measured_throughput_above_1x() {
    let (project, timeline_id, media, _dir) = project_with_gop_25("kf_throughput.mp4");
    let caches = SharedFrameCache::new();
    caches.insert(media, 0, Arc::new(dummy_frame()));
    let mut pacing = Pacing::default();

    pacing.throughput.insert((media, 1), 10.0);
    assert!(
        !decide(&project, timeline_id, &caches, 1.0, &mut pacing),
        "never at 1x"
    );
    assert!(
        decide(&project, timeline_id, &caches, 2.0, &mut pacing),
        "50 needed: 2x too"
    );
    decide(&project, timeline_id, &caches, 0.0, &mut pacing);
    pacing.throughput.insert((media, 1), 70.0);
    assert!(
        !decide(&project, timeline_id, &caches, 4.0, &mut pacing),
        "100 needed, not 1.5 x 70"
    );
    pacing.throughput.insert((media, 1), 60.0);
    assert!(
        decide(&project, timeline_id, &caches, 4.0, &mut pacing),
        "but 1.5 x 60"
    );

    pacing.throughput.insert((media, 1), 500.0);
    assert!(
        decide(&project, timeline_id, &caches, 4.0, &mut pacing),
        "decided once per playback and speed"
    );
    assert!(
        !decide(&project, timeline_id, &caches, 8.0, &mut pacing),
        "a new speed decides again"
    );
    assert!(!decide(&project, timeline_id, &caches, 0.0, &mut pacing));
    assert!(pacing.keyframes_only.is_empty(), "stopped: forgotten");
}

/// A playback of `media` watched for `secs`, with its frame missing all along.
fn missing_all_along(pacing: &mut Pacing, media: MediaId, secs: u64) {
    let now = Instant::now();
    pacing.presence.insert(
        media,
        Presence {
            since: now - Duration::from_secs(secs),
            checked: now,
            was_missing: true,
            missing: Duration::from_secs(secs),
        },
    );
}

/// No throughput yet: the frame at the playhead missing for long enough sends
/// the media to keyframes, and the next playback at that speed starts there.
#[test]
fn keyframes_only_starts_when_the_frame_at_the_playhead_stays_missing() {
    let (project, timeline_id, media, _dir) = project_with_gop_25("kf_stall.mp4");
    let caches = SharedFrameCache::new();
    let mut pacing = Pacing::default();

    assert!(
        !decide(&project, timeline_id, &caches, 4.0, &mut pacing),
        "missing, not for long"
    );
    missing_all_along(&mut pacing, media, 3);
    pacing
        .missing_since
        .insert(media, Instant::now() - Duration::from_millis(200));
    assert!(
        !decide(&project, timeline_id, &caches, 4.0, &mut pacing),
        "under 250 ms"
    );
    pacing
        .missing_since
        .insert(media, Instant::now() - Duration::from_millis(260));
    assert!(decide(&project, timeline_id, &caches, 4.0, &mut pacing));

    decide(&project, timeline_id, &caches, 0.0, &mut pacing);
    caches.insert(media, 0, Arc::new(dummy_frame()));
    assert!(
        decide(&project, timeline_id, &caches, 4.0, &mut pacing),
        "fell behind at 4x before: straight to keyframes"
    );
    assert!(!decide(&project, timeline_id, &caches, 2.0, &mut pacing));
}

/// A long freeze in a playback that otherwise had its frames is a one-off (a
/// cut, the disk): keyframes for the rest of it would be worse.
#[test]
fn keyframes_only_ignores_a_one_off_freeze() {
    let (project, timeline_id, media, _dir) = project_with_gop_25("kf_one_off.mp4");
    let caches = SharedFrameCache::new();
    let mut pacing = Pacing::default();
    let now = Instant::now();
    pacing
        .missing_since
        .insert(media, now - Duration::from_secs(2));
    pacing.presence.insert(
        media,
        Presence {
            since: now - Duration::from_secs(20),
            checked: now - Duration::from_secs(2),
            was_missing: true,
            missing: Duration::ZERO,
        },
    );
    assert!(
        !decide(&project, timeline_id, &caches, 4.0, &mut pacing),
        "2 s missing in 20"
    );

    pacing.presence.get_mut(&media).unwrap().missing = Duration::from_secs(12);
    assert!(
        decide(&project, timeline_id, &caches, 4.0, &mut pacing),
        "14 s in 20"
    );
}

/// The first frame of a clip coming late, at a cut, says nothing about the
/// decoder keeping up: 2 s of the media's playback are watched first.
#[test]
fn keyframes_only_ignores_a_freeze_at_the_start_of_a_clip() {
    let (project, timeline_id, media, _dir) = project_with_gop_25("kf_clip_start.mp4");
    let caches = SharedFrameCache::new();
    let mut pacing = Pacing::default();
    let now = Instant::now();
    pacing
        .missing_since
        .insert(media, now - Duration::from_millis(400));
    pacing
        .presence
        .insert(media, Presence::new(now - Duration::from_millis(400)));

    assert!(!decide(&project, timeline_id, &caches, 4.0, &mut pacing));
    assert!(pacing.fell_behind.is_empty(), "nothing remembered");
}

#[test]
fn presence_counts_the_time_the_frame_was_missing() {
    let start = Instant::now();
    let mut presence = Presence::new(start);
    presence.was_missing = false;
    let share = presence.missing_share(start + Duration::from_secs(1), true);
    assert_eq!(share, 0.0, "shown for the first second");
    let share = presence.missing_share(start + Duration::from_secs(4), false);
    assert!((share - 0.75).abs() < 1e-9, "{share}");
}

/// A GOP of 250 frames at 4x on 25 fps: keyframes only would show one every
/// 2.5 s, so a shorter freeze in normal playback is still the lesser evil.
#[test]
fn keyframes_only_waits_for_a_freeze_longer_than_the_gap_between_keyframes() {
    let (project, timeline_id, media, _dir) = project_with_gop_25("kf_long_gop.mp4");
    let caches = SharedFrameCache::new();
    let mut pacing = Pacing::default();
    let timeline = &project.timelines[timeline_id];
    let segments = collect_media_segments(&project, timeline, 0, 75);
    let gops = HashMap::from([(media, 250)]);
    let mut decide_after = |ms: u64| {
        pacing.keyframes_only.clear();
        missing_all_along(&mut pacing, media, 3);
        pacing
            .missing_since
            .insert(media, Instant::now() - Duration::from_millis(ms));
        !keyframes_only_media(
            &project,
            25.0,
            &segments,
            &caches,
            None,
            &gops,
            0,
            4.0,
            true,
            &mut pacing,
        )
        .is_empty()
    };
    assert!(!decide_after(1000));
    assert!(decide_after(2600));
}

#[test]
fn keyframes_only_is_off_when_disabled_and_for_images() {
    let (project, timeline_id, media, _dir) = project_with_gop_25("kf_disabled.mp4");
    let caches = SharedFrameCache::new();
    let mut pacing = Pacing::default();
    pacing.fell_behind.insert((media, 4.0f64.to_bits()));
    let timeline = &project.timelines[timeline_id];
    let segments = collect_media_segments(&project, timeline, 0, 75);
    assert!(
        keyframes_only_media(
            &project,
            25.0,
            &segments,
            &caches,
            None,
            &HashMap::new(),
            0,
            4.0,
            false,
            &mut pacing
        )
        .is_empty()
    );

    let (_dir_path, path) = make_test_image("vv-app-render-ahead-test", "kf_still.png");
    let mut project = Project::default();
    let image = project.media_pool.insert(MediaItem {
        path,
        meta: MediaMeta {
            duration_frames: vv_core::IMAGE_DURATION_FRAMES,
            fps: vv_media::IMAGE_FPS,
            width: 320,
            height: 240,
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
    pacing.fell_behind.insert((image, 4.0f64.to_bits()));
    let timeline_id = project.timelines.insert(timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![media_clip(1, image, 0, 60)],
        ..Track::new(TrackKind::Video)
    }]));
    assert!(!decide(&project, timeline_id, &caches, 4.0, &mut pacing));
}

/// Keyframes only: the one before the playhead and those in the window, and
/// no frame in between; the one before survives the next cycle's eviction.
#[test]
fn walk_and_fill_in_keyframe_mode_caches_only_keyframes() {
    let (project, timeline_id, media, _dir) = project_with_gop_25("kf_fill.mp4");
    let caches = SharedFrameCache::new();
    let mut open = HashMap::new();
    let pacing = RefCell::new(Pacing::default());
    pacing
        .borrow_mut()
        .fell_behind
        .insert((media, 4.0f64.to_bits()));
    let target = playing_target(30, 4.0);

    let outcome = fill_paced(
        &project,
        timeline_id,
        &caches,
        &mut open,
        30,
        &target,
        &pacing,
    );

    assert!(outcome.caught_up);
    assert!(caches.keyframes_only(media));
    let cached: Vec<FrameIdx> = (0..200).filter(|&i| caches.contains(media, i)).collect();
    assert_eq!(cached, [25, 50, 75, 100], "the window reaches 105");

    store_secs(&target.speed, 4.0);
    target.frame.store(40, Ordering::Relaxed);
    fill_paced(
        &project,
        timeline_id,
        &caches,
        &mut open,
        40,
        &target,
        &pacing,
    );
    assert!(caches.contains(media, 25), "still standing in at frame 40");
    assert_eq!(
        caches.get_near(media, 40, 0, 199).map(|(_, how)| how),
        Some(vv_media::Served::Keyframe)
    );
}

fn landed_at_50(
    file_name: &str,
) -> (
    vv_media::test_support::TempDir,
    Project,
    MediaId,
    OpenDecoder,
) {
    let (project, _, media, dir) = project_with_gop_25(file_name);
    let path = project.media_pool[media].path.clone();
    let mut decoder = Decoder::open(&path).unwrap();
    decoder.seek_to_time(70.0 / 25.0).unwrap();
    (
        dir,
        project,
        media,
        OpenDecoder::fresh(decoder, path, false),
    )
}

fn paused_context<'a>(
    project: &'a Project,
    caches: &'a SharedFrameCache,
    target: &'a LiveTarget,
    pacing: &'a RefCell<Pacing>,
) -> FillContext<'a> {
    FillContext {
        project,
        caches,
        went_backward: false,
        cache_budget_bytes: usize::MAX,
        from_frame: 70,
        proxy: None,
        preview_divisor: 1,
        target,
        jumps: 0,
        window: &[],
        hw_priority: HwPriority::Normal,
        keyframe_first: true,
        pacing,
        decide_by: None,
    }
}

/// Paused, a seek whose way to the frame is long shows the keyframe it
/// landed on first, as a stand-in.
#[test]
fn a_slow_seek_while_paused_shows_its_keyframe_first() {
    // The directory comes first so it drops after the decoder.
    let (_dir, project, media, mut od) = landed_at_50("kf_scrub.mp4");
    let caches = SharedFrameCache::new();
    let pacing = RefCell::new(Pacing::default());
    pacing.borrow_mut().throughput.insert((media, 1), 10.0);
    let target = LiveTarget::still(70);

    show_landing_first(
        &paused_context(&project, &caches, &target, &pacing),
        &mut od,
        media,
        20,
    );

    assert!(caches.contains(media, 50));
    assert!(caches.keyframes_only(media));
    assert!(pacing.borrow().scrub_keyframes.contains(&media));
    assert_eq!(od.decoder.landing(), Some(50), "still to be handed out");
}

#[test]
fn a_quick_seek_while_paused_goes_straight_to_the_frame() {
    // The directory comes first so it drops after the decoder.
    let (_dir, project, media, mut od) = landed_at_50("kf_scrub_quick.mp4");
    let caches = SharedFrameCache::new();
    let pacing = RefCell::new(Pacing::default());
    pacing.borrow_mut().throughput.insert((media, 1), 10_000.0);
    let target = LiveTarget::still(70);

    show_landing_first(
        &paused_context(&project, &caches, &target, &pacing),
        &mut od,
        media,
        20,
    );

    assert!(!caches.contains(media, 50));
    assert!(!caches.keyframes_only(media));
}

/// Once every frame of the window is there no keyframe stands in any more.
#[test]
fn a_settled_window_while_paused_drops_the_stand_in() {
    let (project, timeline_id, media, _dir) = project_with_gop_25("kf_scrub_settled.mp4");
    let caches = SharedFrameCache::new();
    let mut open = HashMap::new();
    let pacing = RefCell::new(Pacing::default());
    pacing.borrow_mut().throughput.insert((media, 1), 10.0);
    let target = playing_target(70, 0.0);

    let outcome = fill_paced(
        &project,
        timeline_id,
        &caches,
        &mut open,
        70,
        &target,
        &pacing,
    );

    assert!(outcome.caught_up);
    assert!(caches.contains(media, 70));
    assert!(pacing.borrow().scrub_keyframes.is_empty());
    fill_paced(
        &project,
        timeline_id,
        &caches,
        &mut open,
        70,
        &target,
        &pacing,
    );
    assert!(!caches.keyframes_only(media));
}

#[test]
fn frame_for_reports_a_keyframe_standing_in() {
    let mut project = Project::default();
    let timeline_id = project.timelines.insert(timeline_with(vec![]));
    let (media, _) = two_media_ids();
    let mut ra = RenderAhead::spawn(project.clone(), timeline_id, 1 << 30, None, 3.0, 2.0);
    ra.shared.caches.insert(media, 10, Arc::new(dummy_frame()));
    let clip = media_clip(1, media, 0, 100);
    use crate::frame_provider::FrameProvider;

    assert!(ra.frame_for(&project, &clip, 10).unwrap().is_some());
    assert!(!ra.take_served_keyframe());
    ra.shared.caches.set_keyframes_only(media, true);
    assert!(ra.frame_for(&project, &clip, 30).unwrap().is_some());
    assert!(ra.take_served_keyframe());
    assert!(!ra.take_served_keyframe(), "taken");
}

#[test]
fn keyframe_spans_answer_only_where_a_seek_proved_no_keyframe() {
    let (media, other) = two_media_ids();
    let mut pacing = Pacing::default();
    pacing.record_keyframe_span(media, 25, 30);
    pacing.record_keyframe_span(media, 25, 45);
    pacing.record_keyframe_span(media, 50, 60);

    assert_eq!(pacing.keyframe_span_at(media, 40), Some((25, 45)));
    assert_eq!(pacing.keyframe_span_at(media, 46), None, "unproven past 45");
    assert_eq!(pacing.keyframe_span_at(media, 55), Some((50, 60)));
    assert_eq!(pacing.keyframe_span_at(media, 10), None);
    assert_eq!(pacing.keyframe_span_at(other, 40), None);

    for k in 0..(MAX_KEYFRAME_SPANS as FrameIdx + 10) {
        pacing.record_keyframe_span(media, 1000 + k, 1000 + k);
    }
    assert_eq!(pacing.keyframe_spans[&media].len(), MAX_KEYFRAME_SPANS);
    assert_eq!(pacing.keyframe_span_at(media, 40), None, "the oldest went");
}

/// With a stride the decoder skips the frames between two multiples: a
/// segment starting on one of them (39, stride 4) is no hole behind a decoder
/// already at 40. Taking it for one sought back a GOP every cycle.
#[test]
fn position_decoder_sees_no_hole_in_the_frames_a_stride_skips() {
    let (_dir_path, path) = make_test_clip("vv-app-render-ahead-test", "stride_hole.mp4", 3);
    let (media, _) = two_media_ids();
    let caches = SharedFrameCache::new();
    caches.set_stride(media, 4);
    caches.insert(media, 36, Arc::new(dummy_frame()));
    let mut open = HashMap::new();
    let mut od = OpenDecoder::fresh(Decoder::open(&path).unwrap(), path.clone(), false);
    od.next_frame = 40;
    open.insert(media, od);

    let positioned = position_decoder(
        &caches,
        &mut open,
        media,
        &path,
        39,
        false,
        false,
        false,
        HwPriority::Normal,
        None,
    );

    assert_eq!(positioned, Positioned::Reused);
}
