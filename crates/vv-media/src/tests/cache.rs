use super::*;

fn two_media_ids() -> (MediaId, MediaId) {
    use vv_core::{MediaItem, MediaMeta, Project, Rational};
    let mut project = Project::default();
    let item = |w: u32, h: u32| MediaItem {
        path: "dummy.mp4".into(),
        meta: MediaMeta {
            duration_frames: 0,
            fps: Rational::new(25, 1),
            width: w,
            height: h,
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
    let a = project.media_pool.insert(item(320, 240));
    let b = project.media_pool.insert(item(320, 240));
    (a, b)
}

/// Frame with `bytes` total bytes, all in the Y plane (the tests using
/// it only check the byte/eviction accounting, not real pixels — where
/// the bytes land among the three planes does not matter).
fn frame_of_size(bytes: usize) -> Arc<FrameYuv420> {
    Arc::new(FrameYuv420 {
        width: 1,
        height: 1,
        y: vec![0; bytes],
        chroma: crate::decode::Chroma::Planar {
            u: vec![],
            v: vec![],
        },
        chroma_width: 0,
        chroma_height: 0,
        matrix: crate::decode::ColorMatrix::Bt601,
        full_range: false,
        alpha: None,
    })
}

#[test]
fn shared_cache_reconcile_tier_a_drops_frames_outside_every_current_range() {
    let (media_a, _) = two_media_ids();
    let cache = SharedFrameCache::new();
    for idx in [5, 50, 100] {
        cache.insert(media_a, idx, frame_of_size(4));
    }
    // Only 50 is inside the single interval wanted in this cycle.
    let window = [WantedRange {
        media_id: media_a,
        source_start: 40,
        source_end: 60,
        timeline_start: 40,
        rate: vv_core::Rational::one(),
    }];
    cache.reconcile(50, &window, 1_000_000);

    assert_eq!(cache.cached_ranges(media_a), vec![(50, 50)]);
    assert_eq!(cache.bytes_used(), 4);
}

#[test]
fn shared_cache_clear_empties_every_media_and_resets_bytes_used() {
    let (media_a, media_b) = two_media_ids();
    let cache = SharedFrameCache::new();
    cache.insert(media_a, 1, frame_of_size(4));
    cache.insert(media_b, 1, frame_of_size(4));

    cache.clear();

    assert!(cache.cached_ranges(media_a).is_empty());
    assert!(cache.cached_ranges(media_b).is_empty());
    assert_eq!(cache.bytes_used(), 0);
}

#[test]
fn shared_cache_reconcile_tier_a_drops_media_entirely_outside_the_window() {
    let (media_a, media_b) = two_media_ids();
    let cache = SharedFrameCache::new();
    cache.insert(media_a, 10, frame_of_size(4));
    cache.insert(media_b, 10, frame_of_size(4));

    // Only media_a shows up in the window of this cycle.
    let window = [WantedRange {
        media_id: media_a,
        source_start: 0,
        source_end: 20,
        timeline_start: 0,
        rate: vv_core::Rational::one(),
    }];
    cache.reconcile(10, &window, 1_000_000);

    assert!(cache.contains(media_a, 10));
    assert!(!cache.contains(media_b, 10));
}

/// The heart of the subtlety in plans/REFACTOR_PIPELINE.md §2: during a
/// forward fill the frame *at the playhead* is the first inserted — the
/// "least recent" for any classic LRU. An LRU by recency would evict it
/// first when the budget runs short; Tier B must instead evict the frame
/// FARTHEST from the playhead, keeping the closest one even if it is the
/// oldest.
#[test]
fn shared_cache_reconcile_tier_b_evicts_by_distance_not_by_recency() {
    let (media_a, _) = two_media_ids();
    let cache = SharedFrameCache::new();
    let window = [WantedRange {
        media_id: media_a,
        source_start: 0,
        source_end: 99,
        timeline_start: 0,
        rate: vv_core::Rational::one(),
    }];
    // Inserted first (the "least recent"), but it is the frame at the
    // playhead: it must survive.
    cache.insert(media_a, 0, frame_of_size(4));
    // Inserted last (the "most recent"), but it is the farthest from
    // the playhead: it must be the first to go.
    cache.insert(media_a, 90, frame_of_size(4));

    // Budget for a single frame: forces a choice between the two.
    cache.reconcile(0, &window, 4);

    assert!(
        cache.contains(media_a, 0),
        "the frame at the playhead must never be evicted to make room for a farther one"
    );
    assert!(!cache.contains(media_a, 90));
}

/// On a conformed clip (25 fps on a 50 fps timeline) one source frame
/// is worth two timeline frames: the distance must be measured there.
#[test]
fn shared_cache_reconcile_measures_distance_in_timeline_frames() {
    let (media_a, media_b) = two_media_ids();
    let cache = SharedFrameCache::new();
    let window = [
        WantedRange {
            media_id: media_a,
            source_start: 0,
            source_end: 99,
            timeline_start: 0,
            rate: vv_core::Rational::new(2, 1),
        },
        WantedRange {
            media_id: media_b,
            source_start: 0,
            source_end: 99,
            timeline_start: 0,
            rate: vv_core::Rational::one(),
        },
    ];
    // Frame 30 of A = timeline 60; frame 50 of B = timeline 50.
    cache.insert(media_a, 30, frame_of_size(4));
    cache.insert(media_b, 50, frame_of_size(4));
    cache.reconcile(0, &window, 4);
    assert!(cache.contains(media_b, 50));
    assert!(!cache.contains(media_a, 30));
}

/// During playback the frames just played are as close to the playhead as
/// the coming ones: the coming ones must win the budget.
#[test]
fn shared_cache_reconcile_evicts_behind_the_playhead_before_ahead() {
    let (media_a, _) = two_media_ids();
    let cache = SharedFrameCache::new();
    let window = [WantedRange {
        media_id: media_a,
        source_start: 0,
        source_end: 99,
        timeline_start: 0,
        rate: vv_core::Rational::one(),
    }];
    cache.insert(media_a, 40, frame_of_size(4));
    cache.insert(media_a, 70, frame_of_size(4));

    cache.reconcile(50, &window, 4);

    assert!(cache.contains(media_a, 70), "20 frames ahead");
    assert!(!cache.contains(media_a, 40), "10 frames behind");
}

#[test]
fn wanted_range_seen_through_a_compound_clip_lands_on_the_outer_timeline() {
    let (media_a, media_b) = two_media_ids();
    // The compound clip shows nested frames from 10 at outer frame 100, each
    // nested frame lasting two outer ones.
    let outer = WantedRange {
        media_id: media_b,
        source_start: 10,
        source_end: 59,
        timeline_start: 100,
        rate: vv_core::Rational::new(2, 1),
    };
    let nested = WantedRange {
        media_id: media_a,
        source_start: 500,
        source_end: 520,
        timeline_start: 30,
        rate: vv_core::Rational::one(),
    };

    let seen = nested.seen_through(&outer);

    assert_eq!(seen.timeline_start, 140);
    assert_eq!(seen.rate, vv_core::Rational::new(2, 1));
    assert_eq!(seen.sub_range(505, 510).timeline_start, 150);
}

#[test]
fn shared_cache_insert_overwrite_updates_bytes_used_correctly() {
    let (media_a, _) = two_media_ids();
    let cache = SharedFrameCache::new();
    cache.insert(media_a, 0, frame_of_size(100));
    assert_eq!(cache.bytes_used(), 100);
    cache.insert(media_a, 0, frame_of_size(40));
    assert_eq!(
        cache.bytes_used(),
        40,
        "overwriting the same (media, idx) must not add the two sizes"
    );
}

#[test]
fn shared_cache_covers_only_contiguous_ranges() {
    let (media_a, media_b) = two_media_ids();
    let cache = SharedFrameCache::new();
    for idx in [5, 6, 7, 9] {
        cache.insert(media_a, idx, frame_of_size(4));
    }
    assert!(cache.covers(media_a, 5, 7));
    assert!(!cache.covers(media_a, 5, 9), "hole at 8");
    assert!(!cache.covers(media_b, 5, 5));
}

#[test]
fn shared_cache_cached_ranges_filters_by_media() {
    let (media_a, media_b) = two_media_ids();
    let cache = SharedFrameCache::new();
    cache.insert(media_a, 5, frame_of_size(4));
    cache.insert(media_a, 6, frame_of_size(4));
    cache.insert(media_b, 5, frame_of_size(4));

    assert_eq!(cache.cached_ranges(media_a), vec![(5, 6)]);
    assert_eq!(cache.cached_ranges(media_b), vec![(5, 5)]);
}

#[test]
fn shared_cache_with_a_stride_covers_and_ranges_only_count_its_multiples() {
    let (media_a, _) = two_media_ids();
    let cache = SharedFrameCache::new();
    for idx in [10, 12, 14, 20] {
        cache.insert(media_a, idx, frame_of_size(4));
    }
    assert!(
        !cache.covers(media_a, 10, 14),
        "every frame without a stride"
    );

    cache.set_stride(media_a, 2);
    assert!(cache.covers(media_a, 9, 15));
    assert!(
        !cache.covers(media_a, 10, 16),
        "16 is a multiple and missing"
    );
    assert_eq!(cache.cached_ranges(media_a), [(10, 14), (20, 20)]);

    cache.set_stride(media_a, 1);
    assert!(!cache.covers(media_a, 10, 14), "back to every frame");
    assert_eq!(
        cache.cached_ranges(media_a),
        [(10, 10), (12, 12), (14, 14), (20, 20)]
    );
}

#[test]
fn shared_cache_with_a_stride_still_wants_a_range_with_no_multiple_in_it() {
    let (media_a, _) = two_media_ids();
    let cache = SharedFrameCache::new();
    cache.set_stride(media_a, 4);
    assert!(
        cache.covers(media_a, 5, 7),
        "no frame wanted: a decoder that skipped 5 to 7 left no hole"
    );
    assert!(
        !cache.covers_segment(media_a, 5, 7),
        "a freeze frame or a 1-frame clip"
    );
    cache.insert(media_a, 5, frame_of_size(4));
    assert!(cache.covers_segment(media_a, 5, 7));
    assert!(cache.covers_segment(media_a, 6, 5), "empty");
}

#[test]
fn shared_cache_get_near_stays_within_the_stride_and_the_clip() {
    let (media_a, _) = two_media_ids();
    let cache = SharedFrameCache::new();
    cache.insert(media_a, 10, frame_of_size(4));
    cache.insert(media_a, 14, frame_of_size(8));

    assert!(
        cache.get_near(media_a, 11, 0, 100).is_none(),
        "exact without a stride"
    );

    cache.set_stride(media_a, 2);
    assert_eq!(
        cache.get_near(media_a, 11, 0, 100).unwrap().0.byte_len(),
        4,
        "earlier first"
    );
    assert_eq!(
        cache.get_near(media_a, 13, 0, 100).unwrap().0.byte_len(),
        8,
        "12 missing"
    );
    assert!(
        cache.get_near(media_a, 12, 0, 100).is_none(),
        "neither neighbour is cached"
    );
    assert!(
        cache.get_near(media_a, 11, 11, 100).is_none(),
        "10 is outside the clip"
    );
    assert!(cache.get_near(media_a, 9, 0, 100).is_some());
}

#[test]
fn shared_cache_remove_media_forgets_its_stride() {
    let (media_a, _) = two_media_ids();
    let cache = SharedFrameCache::new();
    cache.set_stride(media_a, 3);
    cache.remove_media(media_a);
    assert_eq!(cache.stride(media_a), 1);
}

#[test]
fn shared_cache_get_near_says_how_it_served() {
    let (media_a, _) = two_media_ids();
    let cache = SharedFrameCache::new();
    cache.insert(media_a, 10, frame_of_size(4));
    cache.set_stride(media_a, 2);
    assert_eq!(
        cache.get_near(media_a, 10, 0, 100).unwrap().1,
        Served::Exact
    );
    assert_eq!(
        cache.get_near(media_a, 11, 0, 100).unwrap().1,
        Served::Neighbour
    );
}

/// Keyframes only: the latest frame before, within the clip; at the clip's
/// start, before its first keyframe, the next one inside it rather than a
/// frame of the footage trimmed away.
#[test]
fn shared_cache_with_keyframes_only_stands_in_the_nearest_keyframe_in_the_clip() {
    let (media_a, _) = two_media_ids();
    let cache = SharedFrameCache::new();
    cache.insert(media_a, 5500, frame_of_size(4));
    cache.insert(media_a, 5750, frame_of_size(8));
    cache.insert(media_a, 6000, frame_of_size(12));
    assert!(
        cache.get_near(media_a, 5800, 5607, 6084).is_none(),
        "off: exact only"
    );

    cache.set_keyframes_only(media_a, true);
    let served = |idx| {
        cache
            .get_near(media_a, idx, 5607, 6084)
            .map(|(f, how)| (f.byte_len(), how))
    };
    assert_eq!(served(5800), Some((8, Served::Keyframe)));
    assert_eq!(served(6000), Some((12, Served::Exact)));
    assert_eq!(
        served(5610),
        Some((8, Served::Keyframe)),
        "5500 is trimmed away"
    );

    cache.set_keyframes_only(media_a, false);
    assert!(served(5800).is_none());
}

#[test]
fn shared_cache_keyframes_only_media_replaces_the_whole_set() {
    let (media_a, media_b) = two_media_ids();
    let cache = SharedFrameCache::new();
    assert!(!cache.any_keyframes_only());
    cache.set_keyframes_only(media_a, true);
    cache.set_keyframes_only_media([media_b].into());
    assert!(!cache.keyframes_only(media_a), "out of the set: off");
    assert!(cache.keyframes_only(media_b));
    cache.set_keyframes_only_media(Default::default());
    assert!(!cache.any_keyframes_only());
}

#[test]
fn shared_cache_keyframe_ranges_join_the_keyframes_of_a_media_in_keyframe_mode() {
    let (media_a, _) = two_media_ids();
    let cache = SharedFrameCache::new();
    for idx in [250, 500, 750, 5000] {
        cache.insert(media_a, idx, frame_of_size(4));
    }
    assert!(
        cache.keyframe_ranges(media_a).is_empty(),
        "not in keyframe mode"
    );
    cache.set_keyframes_only(media_a, true);
    assert_eq!(cache.keyframe_ranges(media_a), [(250, 750), (5000, 5000)]);
}

#[test]
#[ignore = "manual measurement, not a correctness assertion"]
fn bench_shared_cache_lookups() {
    use std::hint::black_box;
    let (a, b) = two_media_ids();
    let cache = SharedFrameCache::new();
    let frame = frame_of_size(4);
    for index in 0..10_000 {
        cache.insert(a, index * 100, frame.clone());
        cache.insert(b, index, frame.clone());
    }
    cache.set_keyframes_only(a, true);
    for (name, at, distance) in [
        ("exact", 500_000, 1200),
        ("near", 500_050, 1200),
        ("miss", 1_002_000, 1200),
    ] {
        let start = std::time::Instant::now();
        for _ in 0..20_000 {
            black_box(cache.get_near(
                black_box(a),
                black_box(at),
                black_box(at - distance),
                black_box(at + distance),
            ));
        }
        eprintln!("cache {name}, 20000 lookups: {:?}", start.elapsed());
    }
    let start = std::time::Instant::now();
    for _ in 0..200 {
        black_box(cache.cached_ranges(black_box(a)));
    }
    eprintln!("cache ranges, 200 queries: {:?}", start.elapsed());
}

#[test]
fn ordered_cache_neighbours_match_the_probe_order() {
    let (media, _) = two_media_ids();
    let cache = SharedFrameCache::new();
    let indices = [-3, 0, 6, 10, 17, 31, 90];
    for (n, &index) in indices.iter().enumerate() {
        cache.insert(media, index, frame_of_size(n + 1));
    }
    for stride in [1, 2, 10] {
        cache.set_stride(media, stride);
        for keyframes in [false, true] {
            cache.set_keyframes_only(media, keyframes);
            for (lo, hi) in [(-5, 100), (5, 12), (20, 80), (80, 120)] {
                for idx in -5..100 {
                    let cached = |at| {
                        (lo.min(idx)..=hi.max(idx))
                            .contains(&at)
                            .then(|| indices.iter().position(|&i| i == at).map(|i| i + 1))
                            .flatten()
                    };
                    let expected = cached(idx)
                        .map(|bytes| (bytes, Served::Exact))
                        .or_else(|| {
                            (1..stride)
                                .flat_map(|d| [idx - d, idx + d])
                                .find_map(cached)
                                .map(|bytes| (bytes, Served::Neighbour))
                        })
                        .or_else(|| {
                            keyframes
                                .then(|| {
                                    (1..=MAX_KEYFRAME_DISTANCE)
                                        .map(|d| idx - d)
                                        .chain((1..=MAX_KEYFRAME_DISTANCE).map(|d| idx + d))
                                        .find_map(cached)
                                        .map(|bytes| (bytes, Served::Keyframe))
                                })
                                .flatten()
                        });
                    let actual = cache
                        .get_near(media, idx, lo, hi)
                        .map(|(f, served)| (f.byte_len(), served));
                    assert_eq!(
                        actual, expected,
                        "idx={idx}, stride={stride}, bounds={lo}..={hi}, keyframes={keyframes}"
                    );
                }
            }
        }
    }
}
