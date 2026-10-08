use super::*;
use vv_core::{
    ClipId, CrossTransition, Ease, PushDirection, Rational, Track, TrackKind, Transform,
    TransformTracks, Transition, TransitionKind,
};

struct NoMediaProvider;
impl FrameProvider for NoMediaProvider {
    fn frame_for(
        &mut self,
        _: &Project,
        _: &Clip,
        _: FrameIdx,
    ) -> Result<Option<Arc<FrameYuv420>>, crate::export::ExportError> {
        Ok(None)
    }
}

fn solid_clip(id: u64, start: FrameIdx, len: FrameIdx, zoom: [f32; 2]) -> Clip {
    let mut clip = Clip::from_source_range(
        ClipId(id),
        ClipSource::SolidColor,
        0,
        len,
        start,
        Rational::one(),
    );
    clip.effects.transform = TransformTracks::constant(Transform {
        zoom,
        ..Default::default()
    });
    clip
}

fn position_of(layer: &OwnedLayer) -> [f32; 2] {
    assert!(
        matches!(layer.content, OwnedContent::Solid(_)),
        "expected a Solid layer"
    );
    layer.transform.position
}

/// Bug reported by the user: a compound clip is a timeline like any
/// other, so the areas where its nested timeline has nothing to show
/// must stay transparent — not black, or they would cover what is
/// below in the timeline containing it.
#[test]
fn an_empty_area_of_a_compound_clip_shows_the_layer_below_it() {
    let mut project = Project::default();
    // Nested timeline: a red covering only the left half.
    let mut red = solid_clip(1, 0, 10, [1.0, 1.0]);
    red.effects.color = Some(vv_core::Keyframed::constant(Rgba {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    }));
    red.effects.transform = TransformTracks::constant(Transform {
        crop: [0.0, 0.0, 2.0, 0.0],
        ..Default::default()
    });
    let nested_id = project.timelines.insert(Timeline {
        name: "Nested".into(),
        fps: Rational::new(25, 1),
        resolution: (4, 4),
        tracks: vec![video_track(vec![red])],
        markers: Vec::new(),
        master: Default::default(),
    });
    let compound_media = project
        .media_pool
        .insert(compound_media_item(nested_id, (4, 4), 10));

    // Outer timeline: a blue below, the compound clip above.
    let mut blue = solid_clip(2, 0, 10, [1.0, 1.0]);
    blue.effects.color = Some(vv_core::Keyframed::constant(Rgba {
        r: 0.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    }));
    let compound_clip = Clip::from_source_range(
        ClipId(3),
        ClipSource::Media(compound_media),
        0,
        10,
        0,
        Rational::one(),
    );
    let outer = Timeline {
        name: "Outer".into(),
        fps: Rational::new(25, 1),
        resolution: (4, 4),
        tracks: vec![video_track(vec![blue]), video_track(vec![compound_clip])],
        markers: Vec::new(),
        master: Default::default(),
    };

    let compositor = vv_render::Compositor::new_headless();
    let mut inner = NoMediaProvider;
    let mut provider = GpuCompounds::new(&mut inner, &compositor);
    let mut layers = Vec::new();
    for (track_index, clip) in outer.active_video_clips_at(0) {
        layers.extend(
            track_layers_at(
                &project,
                &outer,
                track_index,
                clip,
                0,
                outer.resolution,
                &mut provider,
            )
            .unwrap(),
        );
    }
    assert_eq!(layers.len(), 2, "the blue and the compound clip");

    let render_layers: Vec<vv_render::Layer> = layers.iter().map(OwnedLayer::as_render).collect();
    let out = compositor
        .render_layers_rgba_transparent(&render_layers, vv_render::OutputFrame::exact(4, 4));
    let px = |x: usize, y: usize| &out[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
    assert_eq!(
        px(0, 1),
        &[255, 0, 0, 255],
        "left: the red of the nested timeline"
    );
    assert_eq!(
        px(3, 1),
        &[0, 0, 255, 255],
        "right: empty in the nested one, the blue below shows"
    );
}

/// An adjustment inside a compound clip only reaches the nested tracks below
/// it: the empty part of the nested timeline stays transparent, so the outer
/// layer below shows through unfiltered.
#[test]
fn an_adjustment_inside_a_compound_clip_leaves_the_outer_timeline_alone() {
    let mut project = Project::default();
    let mut red = solid_clip(1, 0, 10, [1.0, 1.0]);
    red.effects.color = Some(vv_core::Keyframed::constant(Rgba {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    }));
    red.effects.transform = TransformTracks::constant(Transform {
        crop: [0.0, 0.0, 2.0, 0.0],
        ..Default::default()
    });
    let mut adjustment =
        Clip::from_source_range(ClipId(4), ClipSource::Adjustment, 0, 10, 0, Rational::one());
    adjustment.effects.filters = vec![vv_core::ClipFilter::new(vv_core::FilterKind::Grayscale)];
    let nested_id = project.timelines.insert(Timeline {
        name: "Nested".into(),
        fps: Rational::new(25, 1),
        resolution: (4, 4),
        tracks: vec![video_track(vec![red]), video_track(vec![adjustment])],
        markers: Vec::new(),
        master: Default::default(),
    });
    let compound_media = project
        .media_pool
        .insert(compound_media_item(nested_id, (4, 4), 10));

    let mut blue = solid_clip(2, 0, 10, [1.0, 1.0]);
    blue.effects.color = Some(vv_core::Keyframed::constant(Rgba {
        r: 0.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    }));
    let compound_clip = Clip::from_source_range(
        ClipId(3),
        ClipSource::Media(compound_media),
        0,
        10,
        0,
        Rational::one(),
    );
    let outer = Timeline {
        name: "Outer".into(),
        fps: Rational::new(25, 1),
        resolution: (4, 4),
        tracks: vec![video_track(vec![blue]), video_track(vec![compound_clip])],
        markers: Vec::new(),
        master: Default::default(),
    };

    let compositor = vv_render::Compositor::new_headless();
    let mut inner = NoMediaProvider;
    let mut provider = GpuCompounds::new(&mut inner, &compositor);
    let mut layers = Vec::new();
    for (track_index, clip) in outer.active_video_clips_at(0) {
        layers.extend(
            track_layers_at(
                &project,
                &outer,
                track_index,
                clip,
                0,
                outer.resolution,
                &mut provider,
            )
            .unwrap(),
        );
    }
    let render_layers: Vec<vv_render::Layer> = layers.iter().map(OwnedLayer::as_render).collect();
    let out = compositor
        .render_layers_rgba_transparent(&render_layers, vv_render::OutputFrame::exact(4, 4));
    let px = |x: usize, y: usize| &out[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
    let left = px(0, 1);
    assert!(
        left[0] == left[1] && left[1] == left[2] && left[0] > 0,
        "the nested red, filtered: {left:?}"
    );
    assert_eq!(px(3, 1), &[0, 0, 255, 255], "the outer blue, untouched");
}

/// Until a media inside the nested timeline is ready, the compound clip
/// produces no layer: better no frame than a half-composed
/// frame.
#[test]
fn a_compound_clip_has_no_layer_until_its_nested_media_is_ready() {
    let mut project = Project::default();
    let missing_media = project.media_pool.insert(vv_core::MediaItem {
        path: "a.mp4".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 10,
            fps: Rational::new(25, 1),
            width: 4,
            height: 4,
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
    let nested_id = project.timelines.insert(Timeline {
        name: "Nested".into(),
        fps: Rational::new(25, 1),
        resolution: (4, 4),
        tracks: vec![video_track(vec![Clip::from_source_range(
            ClipId(1),
            ClipSource::Media(missing_media),
            0,
            10,
            0,
            Rational::one(),
        )])],
        markers: Vec::new(),
        master: Default::default(),
    });
    let compound_media = project
        .media_pool
        .insert(compound_media_item(nested_id, (4, 4), 10));
    let clip = Clip::from_source_range(
        ClipId(2),
        ClipSource::Media(compound_media),
        0,
        10,
        0,
        Rational::one(),
    );
    let outer = Timeline {
        name: "Outer".into(),
        fps: Rational::new(25, 1),
        resolution: (4, 4),
        tracks: vec![video_track(vec![clip.clone()])],
        markers: Vec::new(),
        master: Default::default(),
    };

    let compositor = vv_render::Compositor::new_headless();
    let mut inner = NoMediaProvider;
    let mut provider = GpuCompounds::new(&mut inner, &compositor);
    let layers = track_layers_at(
        &project,
        &outer,
        0,
        &clip,
        0,
        outer.resolution,
        &mut provider,
    )
    .unwrap();

    assert!(
        layers.is_empty(),
        "the nested media is not cached: no layer"
    );
}

fn video_track(clips: Vec<Clip>) -> Track {
    Track {
        kind: TrackKind::Video,
        clips,
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }
}

fn compound_media_item(
    nested: vv_core::TimelineId,
    size: (u32, u32),
    duration: FrameIdx,
) -> vv_core::MediaItem {
    vv_core::MediaItem {
        path: "Compound Clip 1".into(),
        meta: vv_core::MediaMeta {
            duration_frames: duration,
            fps: Rational::new(25, 1),
            width: size.0,
            height: size.1,
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
    }
}

/// Reproduces the bug reported by the user: the right clip of the crossing,
/// zoomed 2x, must stay off screen for the whole first half of the
/// window, not pop in and then merely "pan" — see
/// `push_clearance` in vv-core for the derivation of the 1.5 factor.
#[test]
fn crossing_offsets_clear_a_zoomed_clip_fully_off_screen() {
    let left = solid_clip(1, 0, 100, [1.0, 1.0]);
    let right = solid_clip(2, 100, 100, [2.0, 2.0]);
    let track = Track {
        kind: TrackKind::Video,
        clips: vec![left.clone(), right.clone()],
        muted: false,
        solo: false,
        locked: false,
        crossings: vec![CrossTransition {
            left_clip: ClipId(1),
            right_clip: ClipId(2),
            transition: Transition {
                kind: TransitionKind::Push,
                duration: 20,
                direction: PushDirection::Right,
                ease: Ease::None,
                curve: 0.0,
            },
        }],
        mix: Default::default(),
        armed: Default::default(),
    };
    let timeline = Timeline {
        name: "t".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![track],
        markers: Vec::new(),
        master: Default::default(),
    };
    let project = Project::default();
    let mut provider = NoMediaProvider;
    let frame_size = (1920.0, 1080.0);

    // Start of the window: the left one is still entirely in place, the zoomed
    // right one must vanish past `frame_size.0`, not stop at `frame_size.0`.
    let layers = track_layers_at(
        &project,
        &timeline,
        0,
        &left,
        90,
        timeline.resolution,
        &mut provider,
    )
    .unwrap();
    assert_eq!(position_of(&layers[0]), [0.0, 0.0]);
    assert_eq!(position_of(&layers[1]), [-1.5 * frame_size.0, 0.0]);

    // Near the end of the window (109, the last frame before the window
    // [90, 110) closes): the left one (zoom 1x) is almost entirely out
    // with the "full" clearance, the right one (zoom 2x) is almost entirely
    // in place.
    let layers = track_layers_at(
        &project,
        &timeline,
        0,
        &left,
        109,
        timeline.resolution,
        &mut provider,
    )
    .unwrap();
    let progress = 19.0 / 20.0;
    assert_eq!(position_of(&layers[0]), [progress * frame_size.0, 0.0]);
    assert_eq!(
        position_of(&layers[1]),
        [-(1.0 - progress) * 1.5 * frame_size.0, 0.0]
    );
}

fn gray_frame() -> Arc<FrameYuv420> {
    Arc::new(FrameYuv420 {
        width: 2,
        height: 2,
        y: vec![128; 4],
        chroma: vv_media::Chroma::Planar {
            u: vec![128],
            v: vec![128],
        },
        chroma_width: 1,
        chroma_height: 1,
        matrix: vv_core::ColorMatrix::Bt709,
        full_range: false,
        alpha: None,
    })
}

fn video_layer(frame: Arc<FrameYuv420>, opacity: f32) -> OwnedLayer {
    OwnedLayer {
        content: OwnedContent::Video {
            frame,
            source_size: (2, 2),
        },
        transform: Transform::default(),
        opacity,
        filters: Vec::new(),
        blend: vv_core::BlendMode::Normal,
        masks: Vec::new(),
    }
}

#[test]
fn the_same_frame_with_the_same_parameters_renders_the_same() {
    let frame = gray_frame();
    assert!(renders_same(
        &[video_layer(frame.clone(), 1.0)],
        &[video_layer(frame, 1.0)]
    ));
    assert!(renders_same(&[], &[]));
}

/// Identity, not content: a new decode of identical pixels still recomposes.
#[test]
fn a_different_frame_or_parameter_renders_differently() {
    let frame = gray_frame();
    let shown = [video_layer(frame.clone(), 1.0)];
    assert!(!renders_same(&shown, &[video_layer(gray_frame(), 1.0)]));
    assert!(!renders_same(&shown, &[video_layer(frame.clone(), 0.5)]));
    assert!(!renders_same(
        &shown,
        &[video_layer(frame.clone(), 1.0), video_layer(frame, 1.0)]
    ));
}

#[test]
fn a_nan_parameter_never_renders_the_same() {
    let frame = gray_frame();
    let layer = || video_layer(frame.clone(), f32::NAN);
    assert!(!renders_same(&[layer()], &[layer()]));
}

/// The export closes the decoders of the clips not in this list: those inside
/// a compound clip must be in it, or they reopen and seek on every frame.
#[test]
fn the_clips_decoded_at_a_frame_include_those_of_the_nested_timelines() {
    let mut project = Project::default();
    let nested_id = project.timelines.insert(Timeline {
        name: "Nested".into(),
        fps: Rational::new(25, 1),
        resolution: (4, 4),
        tracks: vec![video_track(vec![
            solid_clip(1, 0, 5, [1.0, 1.0]),
            solid_clip(2, 5, 5, [1.0, 1.0]),
        ])],
        markers: Vec::new(),
        master: Default::default(),
    });
    let compound_media = project
        .media_pool
        .insert(compound_media_item(nested_id, (4, 4), 10));
    let outer = Timeline {
        name: "Outer".into(),
        fps: Rational::new(25, 1),
        resolution: (4, 4),
        tracks: vec![video_track(vec![Clip::from_source_range(
            ClipId(3),
            ClipSource::Media(compound_media),
            0,
            10,
            20,
            Rational::one(),
        )])],
        markers: Vec::new(),
        master: Default::default(),
    };

    assert_eq!(clips_decoded_at(&project, &outer, 5), Vec::new());
    assert_eq!(
        clips_decoded_at(&project, &outer, 22),
        vec![ClipId(3), ClipId(1)]
    );
    assert_eq!(
        clips_decoded_at(&project, &outer, 27),
        vec![ClipId(3), ClipId(2)]
    );
}
