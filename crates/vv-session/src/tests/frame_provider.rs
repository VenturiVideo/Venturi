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

/// A reduced preview composes a compound clip at a fraction of its timeline
/// and the outer frame at the same fraction: the picture stays the same.
#[test]
fn a_reduced_preview_composes_a_compound_clip_smaller_with_the_same_picture() {
    let mut project = Project::default();
    let mut red = solid_clip(1, 0, 10, [1.0, 1.0]);
    red.effects.color = Some(vv_core::Keyframed::constant(Rgba {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    }));
    red.effects.transform = TransformTracks::constant(Transform {
        crop: [0.0, 0.0, 4.0, 0.0],
        ..Default::default()
    });
    let nested_id = project.timelines.insert(Timeline {
        name: "Nested".into(),
        fps: Rational::new(25, 1),
        resolution: (8, 8),
        tracks: vec![video_track(vec![red])],
        markers: Vec::new(),
        master: Default::default(),
    });
    let compound_media = project
        .media_pool
        .insert(compound_media_item(nested_id, (8, 8), 10));
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
        resolution: (8, 8),
        tracks: vec![video_track(vec![blue]), video_track(vec![compound_clip])],
        markers: Vec::new(),
        master: Default::default(),
    };

    let compositor = vv_render::Compositor::new_headless();
    let mut inner = NoMediaProvider;
    let mut provider = GpuCompounds::new(&mut inner, &compositor).with_preview_divisor(2);
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
    let compound_width = layers.iter().find_map(|l| match &l.content {
        OwnedContent::Texture { texture, .. } => Some(texture.width()),
        _ => None,
    });
    assert_eq!(compound_width, Some(4), "the compound at half its 8 px");

    let render_layers: Vec<vv_render::Layer> = layers.iter().map(OwnedLayer::as_render).collect();
    let out = compositor.render_layers_rgba_transparent(
        &render_layers,
        vv_render::OutputFrame::exact(8, 8).reduced(2),
    );
    let px = |x: usize, y: usize| &out[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
    assert_eq!(px(0, 1), &[255, 0, 0, 255], "left: the nested red");
    assert_eq!(px(3, 1), &[0, 0, 255, 255], "right: the blue below");
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
    adjustment.effects.filters = vec![vv_core::ClipFilter {
        grade: vv_core::GradeTracks::constant(vv_core::GradePreset::BlackAndWhite.value()),
        ..vv_core::ClipFilter::new(vv_core::FilterKind::ColorCorrection)
    }];
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

#[test]
fn crossing_dissolve_fades_the_right_clip_in_over_the_left_one() {
    let left = solid_clip(1, 0, 100, [1.0, 1.0]);
    let right = solid_clip(2, 100, 100, [1.0, 1.0]);
    let mut track = Track::new(TrackKind::Video);
    track.clips = vec![left.clone(), right];
    track.crossings = vec![CrossTransition {
        left_clip: ClipId(1),
        right_clip: ClipId(2),
        transition: Transition {
            kind: TransitionKind::CrossDissolve,
            duration: 20,
            direction: PushDirection::Right,
            ease: Ease::None,
            curve: 0.0,
        },
    }];
    let timeline = Timeline {
        name: "t".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![track],
        markers: Vec::new(),
        master: Default::default(),
    };
    let layers = track_layers_at(
        &Project::default(),
        &timeline,
        0,
        &left,
        100,
        timeline.resolution,
        &mut NoMediaProvider,
    )
    .unwrap();

    assert_eq!(layers.len(), 2);
    assert_eq!(layers[0].opacity, 1.0);
    assert_eq!(layers[1].opacity, 0.5);
    assert_eq!(position_of(&layers[0]), [0.0, 0.0]);
    assert_eq!(position_of(&layers[1]), [0.0, 0.0]);
}

/// Without a GPU to morph on, a Smooth Cut dissolves between the frame
/// before its window and the one after, both held still.
#[test]
fn a_smooth_cut_without_a_gpu_dissolves_between_the_held_sides() {
    let left = solid_clip(1, 0, 100, [1.0, 1.0]);
    let right = solid_clip(2, 100, 100, [1.0, 1.0]);
    let mut track = Track::new(TrackKind::Video);
    track.clips = vec![left.clone(), right];
    track.crossings = vec![CrossTransition {
        left_clip: ClipId(1),
        right_clip: ClipId(2),
        transition: Transition {
            kind: TransitionKind::SmoothCut,
            duration: 4,
            direction: PushDirection::Right,
            ease: Ease::None,
            curve: 0.0,
        },
    }];
    let timeline = Timeline {
        name: "t".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![track],
        markers: Vec::new(),
        master: Default::default(),
    };
    let layers = track_layers_at(
        &Project::default(),
        &timeline,
        0,
        &left,
        99,
        timeline.resolution,
        &mut NoMediaProvider,
    )
    .unwrap();

    assert_eq!(layers.len(), 2);
    assert_eq!(layers[0].opacity, 1.0);
    assert_eq!(layers[1].opacity, 0.25);
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

fn colored_solid(id: u64, color: Rgba) -> Clip {
    let mut clip = solid_clip(id, 0, 10, [1.0, 1.0]);
    clip.effects.color = Some(vv_core::Keyframed::constant(color));
    clip
}

fn stack_at_0(timeline: &Timeline, provider: &mut dyn FrameProvider) -> Vec<OwnedLayer> {
    let project = Project::default();
    let mut groups = Vec::new();
    for (track_index, clip) in timeline.active_video_clips_at(0) {
        let layers = track_layers_at(
            &project,
            timeline,
            track_index,
            clip,
            0,
            timeline.resolution,
            provider,
        )
        .unwrap();
        groups.push((clip.effects.matte, layers));
    }
    stack_layers(groups, provider, timeline.resolution)
}

const RED: Rgba = Rgba {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};
const BLUE: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 1.0,
    a: 1.0,
};

/// Blue, red, and above them a black matte covering the left half.
fn matted_timeline(matte_mode: vv_core::MatteMode) -> Timeline {
    let mut matte = colored_solid(3, Rgba::BLACK);
    matte.effects.transform = TransformTracks::constant(Transform {
        crop: [0.0, 0.0, 2.0, 0.0],
        ..Default::default()
    });
    matte.effects.matte = Some(matte_mode);
    Timeline {
        name: "Matte".into(),
        fps: Rational::new(25, 1),
        resolution: (4, 4),
        tracks: vec![
            video_track(vec![colored_solid(1, BLUE)]),
            video_track(vec![colored_solid(2, RED)]),
            video_track(vec![matte]),
        ],
        markers: Vec::new(),
        master: Default::default(),
    }
}

#[test]
fn a_matte_cuts_out_the_track_below_and_is_not_shown() {
    let compositor = vv_render::Compositor::new_headless();
    let mut inner = NoMediaProvider;
    let mut provider = GpuCompounds::new(&mut inner, &compositor);
    let layers = stack_at_0(&matted_timeline(vv_core::MatteMode::Alpha), &mut provider);
    assert_eq!(layers.len(), 2, "the blue and the cut out red");

    let render_layers: Vec<vv_render::Layer> = layers.iter().map(OwnedLayer::as_render).collect();
    let out = compositor
        .render_layers_rgba_transparent(&render_layers, vv_render::OutputFrame::exact(4, 4));
    let px = |x: usize, y: usize| &out[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
    assert_eq!(px(0, 1), &[255, 0, 0, 255], "left: inside the matte, red");
    assert_eq!(
        px(3, 1),
        &[0, 0, 255, 255],
        "right: outside, the blue below"
    );
}

#[test]
fn a_matte_with_nothing_below_shows_nothing() {
    let mut timeline = matted_timeline(vv_core::MatteMode::Alpha);
    timeline.tracks.drain(..2);
    let compositor = vv_render::Compositor::new_headless();
    let mut inner = NoMediaProvider;
    let mut provider = GpuCompounds::new(&mut inner, &compositor);
    assert!(stack_at_0(&timeline, &mut provider).is_empty());
}

#[test]
fn without_the_gpu_a_matte_leaves_the_track_below_uncut() {
    let mut timeline = matted_timeline(vv_core::MatteMode::Alpha);
    timeline.tracks[1].clips[0].effects.blend_mode = vv_core::BlendMode::Screen;
    let layers = stack_at_0(&timeline, &mut NoMediaProvider);
    assert_eq!(layers.len(), 2);
    assert!(matches!(layers[1].content, OwnedContent::Solid(c) if c == RED));
    assert_eq!(layers[1].blend, vv_core::BlendMode::Screen);
}

#[test]
fn a_title_layer_shows_the_text_of_its_frame() {
    let mut clip = solid_clip(1, 0, 20, [1.0, 1.0]);
    clip.source = ClipSource::Text;
    let mut title = TitleParams::default();
    title
        .content
        .upsert(10, "Later".into(), vv_core::Interpolation::Hold);
    clip.effects.title = Some(title);
    let text_at = |frame| {
        let layer = clip_layer(
            &Project::default(),
            &clip,
            frame,
            (4, 4),
            vv_core::Rational::new(25, 1),
            &mut NoMediaProvider,
        )
        .unwrap()
        .unwrap();
        match layer.content {
            OwnedContent::Text(title) => title.display_text(),
            _ => panic!("not a title"),
        }
    };
    assert_eq!(text_at(9), "Later", "before the first keyframe, its text");
    assert_eq!(text_at(15), "Later");
    let mut two = clip.clone();
    two.effects.title.as_mut().unwrap().content.upsert(
        0,
        "First".into(),
        vv_core::Interpolation::Hold,
    );
    let first = clip_layer(
        &Project::default(),
        &two,
        5,
        (4, 4),
        vv_core::Rational::new(25, 1),
        &mut NoMediaProvider,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(first.content, OwnedContent::Text(t) if t.display_text() == "First"));
}

#[test]
fn a_matte_over_an_adjustment_cuts_out_the_adjusted_stack() {
    let mut timeline = matted_timeline(vv_core::MatteMode::Alpha);
    let mut adjustment = solid_clip(2, 0, 10, [1.0, 1.0]);
    adjustment.source = ClipSource::Adjustment;
    let mut darken = vv_core::ClipFilter::new(vv_core::FilterKind::Exposure);
    darken.amount = vv_core::Keyframed::constant(-10.0);
    adjustment.effects.filters.push(darken);
    timeline.tracks[1].clips = vec![adjustment];

    let compositor = vv_render::Compositor::new_headless();
    let mut inner = NoMediaProvider;
    let mut provider = GpuCompounds::new(&mut inner, &compositor);
    let layers = stack_at_0(&timeline, &mut provider);
    let render_layers: Vec<vv_render::Layer> = layers.iter().map(OwnedLayer::as_render).collect();
    let out = compositor
        .render_layers_rgba_transparent(&render_layers, vv_render::OutputFrame::exact(4, 4));
    let px = |x: usize, y: usize| &out[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
    let inside = px(0, 1);
    assert!(
        inside[2] < 40 && inside[3] == 255,
        "left: inside the matte, the darkened blue: {inside:?}"
    );
    assert_eq!(px(3, 1), &[0, 0, 0, 0], "right: outside, nothing");
}

#[test]
fn camera_shake_is_evaluated_for_normal_and_adjustment_layers_and_survives_a_split() {
    let mut clip = solid_clip(1, 0, 100, [1.0; 2]);
    clip.effects
        .filters
        .push(vv_core::ClipFilter::new(vv_core::FilterKind::CameraShake));
    let project = Project::default();
    let fps = Rational::new(25, 1);
    let layer = |clip: &Clip, frame| {
        clip_layer(
            &project,
            clip,
            frame,
            (1920, 1080),
            fps,
            &mut NoMediaProvider,
        )
        .unwrap()
        .unwrap()
    };
    let expected = layer(&clip, 61).transform;
    assert_ne!(expected, Transform::default());
    assert!(
        layer(&clip, 61).filters.is_empty(),
        "shake must not consume pixel filter slots"
    );
    clip.source_offset = 50;
    clip.timeline_start = 50;
    clip.timeline_len = 50;
    assert_eq!(
        layer(&clip, 61).transform,
        expected,
        "splitting must keep the motion phase"
    );
    clip.source = ClipSource::Adjustment;
    assert_eq!(layer(&clip, 61).transform, expected);
    clip.effects.filters[0].enabled = false;
    assert_eq!(layer(&clip, 61).transform, Transform::default());
}

#[test]
fn camera_shake_layer_uses_the_timeline_clock_for_generators() {
    let mut clip = solid_clip(1, 0, 100, [1.0; 2]);
    clip.effects
        .filters
        .push(vv_core::ClipFilter::new(vv_core::FilterKind::CameraShake));
    let project = Project::default();
    let mut provider = NoMediaProvider;
    let a = clip_layer(
        &project,
        &clip,
        25,
        (1920, 1080),
        Rational::new(25, 1),
        &mut provider,
    )
    .unwrap()
    .unwrap();
    let b = clip_layer(
        &project,
        &clip,
        60,
        (1920, 1080),
        Rational::new(60, 1),
        &mut provider,
    )
    .unwrap()
    .unwrap();
    assert_eq!(a.transform, b.transform);
}

#[test]
fn camera_shake_does_not_scale_or_rotate_single_edge_push_offsets() {
    let project = Project::default();
    let size = (1920, 1080);
    let fps = Rational::new(25, 1);
    for direction in PushDirection::ALL {
        let mut clip = solid_clip(1, 0, 100, [0.8, 1.2]);
        clip.effects
            .filters
            .push(vv_core::ClipFilter::new(vv_core::FilterKind::CameraShake));
        let mut transitioning = clip.clone();
        let transition = Transition {
            kind: TransitionKind::Push,
            duration: 20,
            direction,
            ease: Ease::None,
            curve: 0.0,
        };
        transitioning.effects.transition_in = Some(transition.clone());
        transitioning.effects.transition_out = Some(transition);
        for frame in [0, 5, 19, 80, 95, 99] {
            let offset = transitioning.transition_offset_at(frame, (1920.0, 1080.0), [0.8, 1.2]);
            let mut expected = clip_layer(&project, &clip, frame, size, fps, &mut NoMediaProvider)
                .unwrap()
                .unwrap()
                .transform;
            expected.position[0] += offset[0];
            expected.position[1] += offset[1];
            let actual = clip_layer(
                &project,
                &transitioning,
                frame,
                size,
                fps,
                &mut NoMediaProvider,
            )
            .unwrap()
            .unwrap()
            .transform;
            assert_eq!(actual, expected, "{direction:?} at frame {frame}");
        }
    }
}

#[test]
fn crossing_push_offsets_stay_in_timeline_space_when_only_one_side_shakes() {
    let project = Project::default();
    let size = (1920, 1080);
    let fps = Rational::new(25, 1);
    for direction in PushDirection::ALL {
        for shaking_side in 0..2 {
            let mut clips = [
                solid_clip(1, 0, 100, [1.0; 2]),
                solid_clip(2, 100, 100, [1.0; 2]),
            ];
            clips[shaking_side]
                .effects
                .filters
                .push(vv_core::ClipFilter::new(vv_core::FilterKind::CameraShake));
            let crossing = CrossTransition {
                left_clip: clips[0].id,
                right_clip: clips[1].id,
                transition: Transition {
                    kind: TransitionKind::Push,
                    duration: 20,
                    direction,
                    ease: Ease::None,
                    curve: 0.0,
                },
            };
            for frame in [90, 95, 100, 105, 109] {
                let progress = crossing.eased_progress_at(frame, &clips[0], &clips[1]);
                let (left_offset, right_offset) =
                    crossing.offsets(progress, (1920.0, 1080.0), [1.0; 2], [1.0; 2]);
                let layers = crossing_layers(
                    &project,
                    &clips[0],
                    &clips[1],
                    &crossing,
                    frame,
                    size,
                    fps,
                    &mut NoMediaProvider,
                )
                .unwrap();
                assert_eq!(layers.len(), 2);
                for ((clip, layer), offset) in
                    clips.iter().zip(&layers).zip([left_offset, right_offset])
                {
                    let mut expected =
                        clip_layer(&project, clip, frame, size, fps, &mut NoMediaProvider)
                            .unwrap()
                            .unwrap()
                            .transform;
                    expected.position[0] += offset[0];
                    expected.position[1] += offset[1];
                    assert_eq!(
                        layer.transform, expected,
                        "{direction:?}, shaking side {shaking_side}, frame {frame}"
                    );
                }
            }
        }
    }
}

#[test]
fn retiming_deliberately_retimes_the_shake_clock() {
    let project = Project::default();
    let mut normal = solid_clip(1, 100, 400, [1.0; 2]);
    normal
        .effects
        .filters
        .push(vv_core::ClipFilter::new(vv_core::FilterKind::CameraShake));
    let layer = |clip: &Clip, frame| {
        clip_layer(
            &project,
            clip,
            frame,
            (1920, 1080),
            Rational::new(25, 1),
            &mut NoMediaProvider,
        )
        .unwrap()
        .unwrap()
        .transform
    };
    for (speed, equivalent_frame) in [(Rational::new(1, 4), 110), (Rational::new(2, 1), 180)] {
        let mut retimed = Clip::new(
            ClipId(2),
            ClipSource::SolidColor,
            0,
            100,
            400,
            Rational::one(),
            speed,
        );
        retimed.effects = normal.effects.clone();
        assert_eq!(layer(&retimed, 140), layer(&normal, equivalent_frame));
        assert_ne!(layer(&retimed, 140), layer(&normal, 140));
    }
}

#[test]
fn camera_shake_changes_rendered_pixels_and_repeated_renders_match() {
    let compositor = vv_render::Compositor::new_headless();
    let project = Project::default();
    let mut clip = solid_clip(1, 0, 100, [0.5; 2]);
    clip.effects.color = Some(vv_core::Keyframed::constant(RED));
    let mut filter = vv_core::ClipFilter::new(vv_core::FilterKind::CameraShake);
    filter.shake.auto_zoom = false;
    filter.shake.track_mut(vv_core::ShakeParam::Pan).default = 10.0;
    clip.effects.filters.push(filter);
    let render = |frame| {
        let layer = clip_layer(
            &project,
            &clip,
            frame,
            (64, 64),
            Rational::new(25, 1),
            &mut NoMediaProvider,
        )
        .unwrap()
        .unwrap();
        compositor.render_layers_rgba_transparent(
            &[layer.as_render()],
            vv_render::OutputFrame::exact(64, 64),
        )
    };
    let first = render(0);
    let later = render(15);
    assert_ne!(first, later);
    assert!(
        first
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] > 200 && p[3] > 200)
    );
    assert_eq!(first, render(0));
}
