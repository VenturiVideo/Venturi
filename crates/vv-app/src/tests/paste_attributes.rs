use super::*;
use vv_core::{ClipSource, Interpolation, Rational};

fn clip(id: u64, source_in: FrameIdx, source_out: FrameIdx) -> Clip {
    Clip::from_source_range(
        ClipId(id),
        ClipSource::SolidColor,
        source_in,
        source_out,
        0,
        Rational::one(),
    )
}

fn source_clip() -> Clip {
    let mut clip = clip(1, 0, 20);
    clip.effects
        .transform
        .track_mut(TransformParam::ZoomX)
        .default = 2.0;
    clip.effects
        .transform
        .track_mut(TransformParam::Opacity)
        .default = 0.5;
    clip.fade_in = 4;
    clip
}

#[test]
fn only_the_ticked_attributes_are_pasted() {
    let target = clip(2, 0, 20);
    let selected = HashSet::from([Attribute::Zoom]);
    let pasted = merged_attributes(
        &source_clip(),
        &target,
        &selected,
        KeyframeMode::MaintainTiming,
    );

    assert_eq!(
        pasted
            .effects
            .transform
            .track(TransformParam::ZoomX)
            .default,
        2.0
    );
    assert_eq!(
        pasted
            .effects
            .transform
            .track(TransformParam::Opacity)
            .default,
        target
            .effects
            .transform
            .track(TransformParam::Opacity)
            .default,
        "unselected opacity stays that of the destination clip"
    );
    assert_eq!(pasted.fade_in, 0, "unselected fades are left alone");
}

/// The keyframes are in source frames: a target clip starting from
/// another point of its media must keep the same distances from its
/// own start.
#[test]
fn maintain_timing_keeps_the_offsets_from_the_start_of_the_clip() {
    let mut source = source_clip();
    source
        .effects
        .transform
        .track_mut(TransformParam::ZoomX)
        .upsert(5, 3.0, Interpolation::Linear);
    let target = clip(2, 100, 120);

    let pasted = merged_attributes(
        &source,
        &target,
        &HashSet::from([Attribute::Zoom]),
        KeyframeMode::MaintainTiming,
    );

    let keyframes = pasted
        .effects
        .transform
        .track(TransformParam::ZoomX)
        .keyframes();
    assert_eq!(keyframes.len(), 1);
    assert_eq!(keyframes[0].0, 105);
}

#[test]
fn stretch_to_fit_scales_the_keyframes_on_the_target_duration() {
    let mut source = source_clip();
    let track = source.effects.transform.track_mut(TransformParam::ZoomX);
    track.upsert(10, 3.0, Interpolation::Linear);
    track.upsert(15, 4.0, Interpolation::Linear);
    // Half the source: 20 source frames against 40.
    let target = clip(2, 0, 40);

    let pasted = merged_attributes(
        &source,
        &target,
        &HashSet::from([Attribute::Zoom]),
        KeyframeMode::StretchToFit,
    );

    let keyframes = pasted
        .effects
        .transform
        .track(TransformParam::ZoomX)
        .keyframes();
    assert_eq!(
        keyframes.iter().map(|k| k.0).collect::<Vec<_>>(),
        vec![20, 30]
    );
}

/// A keyframe past the end of the target clip has nowhere to go.
#[test]
fn keyframes_outside_the_target_clip_are_dropped() {
    let mut source = source_clip();
    source
        .effects
        .transform
        .track_mut(TransformParam::ZoomX)
        .upsert(15, 3.0, Interpolation::Linear);
    let target = clip(2, 0, 5);

    let pasted = merged_attributes(
        &source,
        &target,
        &HashSet::from([Attribute::Zoom]),
        KeyframeMode::MaintainTiming,
    );

    assert!(
        pasted
            .effects
            .transform
            .track(TransformParam::ZoomX)
            .is_constant()
    );
    assert_eq!(
        pasted
            .effects
            .transform
            .track(TransformParam::ZoomX)
            .default,
        2.0
    );
}

#[test]
fn fades_and_transitions_are_clamped_to_the_target_length() {
    let mut source = source_clip();
    source.fade_out = 8;
    let target = clip(2, 0, 5);

    let pasted = merged_attributes(
        &source,
        &target,
        &HashSet::from([Attribute::Fades]),
        KeyframeMode::MaintainTiming,
    );

    assert_eq!(pasted.fade_in, 4);
    assert_eq!(pasted.fade_out, 5);
}

/// The whole path: copy, select another clip, apply the dialog.
#[test]
fn applying_the_dialog_writes_a_single_undoable_step() {
    let mut app = VenturiApp::default();
    let timeline_id = app.ensure_timeline();
    let track_index = app.session.project.timelines[timeline_id]
        .tracks_of_kind(TrackKind::Video)
        .next()
        .map(|(i, _)| i)
        .expect("the timeline is created with a video track");

    let mut source = clip(0, 0, 20);
    source.id = app.session.project.alloc_clip_id();
    source
        .effects
        .transform
        .track_mut(TransformParam::ZoomX)
        .default = 2.0;
    let source_id = source.id;
    let mut target = clip(0, 0, 20);
    target.id = app.session.project.alloc_clip_id();
    target.timeline_start = 40;
    let target_id = target.id;
    let track = &mut app.session.project.timelines[timeline_id].tracks[track_index];
    track.insert_sorted(source);
    track.insert_sorted(target);

    app.timeline_state.selected.insert((track_index, source_id));
    app.copy_selected_clips();
    app.timeline_state.set_selection(
        std::collections::BTreeSet::from([(track_index, target_id)]),
        None,
    );
    app.paste_attributes_selection = HashSet::from([Attribute::Zoom]);
    app.open_paste_attributes_dialog();
    let dialog = app.paste_attributes.take().expect("the dialog opens");
    app.apply_paste_attributes(&dialog);

    let zoom_of = |app: &VenturiApp, id| {
        app.session.project.timelines[timeline_id]
            .clip(track_index, id)
            .unwrap()
            .effects
            .transform
            .track(TransformParam::ZoomX)
            .default
    };
    assert_eq!(zoom_of(&app, target_id), 2.0);
    app.session.history.undo(&mut app.session.project);
    assert_eq!(zoom_of(&app, target_id), 1.0, "a single undo step");
}

/// The speed goes with the attributes: the target keeps its source range
/// and changes length, what follows it moves.
#[test]
fn pasting_the_speed_retimes_the_target() {
    let mut app = VenturiApp::default();
    let timeline_id = app.ensure_timeline();
    let track_index = app.session.project.timelines[timeline_id]
        .tracks_of_kind(TrackKind::Video)
        .next()
        .map(|(i, _)| i)
        .unwrap();
    let fps = app.session.project.timelines[timeline_id].fps;
    let media = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "a.mp4".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 1000,
            fps,
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
        compound: None,
        folder: None,
    });
    let media_clip = |app: &mut VenturiApp, start| {
        let mut clip = Clip::from_source_range(
            app.session.project.alloc_clip_id(),
            ClipSource::Media(media),
            0,
            100,
            start,
            Rational::one(),
        );
        clip.timeline_start = start;
        clip
    };
    let mut source = media_clip(&mut app, 0);
    source.set_speed(Rational::new(2, 1), Rational::one());
    source.pitch_correction = true;
    let target = media_clip(&mut app, 100);
    let (source_id, target_id) = (source.id, target.id);
    let mut after = clip(0, 0, 10);
    after.id = app.session.project.alloc_clip_id();
    after.timeline_start = 200;
    let after_id = after.id;
    let track = &mut app.session.project.timelines[timeline_id].tracks[track_index];
    track.insert_sorted(source);
    track.insert_sorted(target);
    track.insert_sorted(after);

    app.timeline_state.selected.insert((track_index, source_id));
    app.copy_selected_clips();
    app.timeline_state.set_selection(
        std::collections::BTreeSet::from([(track_index, target_id)]),
        None,
    );
    app.paste_attributes_selection = HashSet::from([Attribute::Speed]);
    app.open_paste_attributes_dialog();
    let dialog = app.paste_attributes.take().expect("the dialog opens");
    app.apply_paste_attributes(&dialog);

    let tl = &app.session.project.timelines[timeline_id];
    let pasted = tl.clip(track_index, target_id).unwrap();
    assert_eq!(
        (pasted.speed(), pasted.pitch_correction),
        (Rational::new(2, 1), true)
    );
    assert_eq!((pasted.timeline_start, pasted.timeline_len), (100, 50));
    assert_eq!(tl.clip(track_index, after_id).unwrap().timeline_start, 150);

    app.session.history.undo(&mut app.session.project);
    let tl = &app.session.project.timelines[timeline_id];
    assert_eq!(
        tl.clip(track_index, target_id).unwrap().speed(),
        Rational::one()
    );
    assert_eq!(
        tl.clip(track_index, after_id).unwrap().timeline_start,
        200,
        "a single undo step"
    );
}

#[test]
fn pasted_filters_bring_their_keyframes_into_the_target_clip() {
    let mut source = source_clip();
    let mut filter = vv_core::ClipFilter::new(vv_core::FilterKind::BoxBlur);
    filter.radius.upsert(5, 30.0, Interpolation::Linear);
    filter
        .direction
        .upsert(5, vv_core::BlurDirection::Vertical, Interpolation::Linear);
    source.effects.filters.push(filter);
    let target = clip(2, 100, 120);

    let pasted = merged_attributes(
        &source,
        &target,
        &HashSet::from([Attribute::Filters]),
        KeyframeMode::MaintainTiming,
    );

    let filter = &pasted.effects.filters[0];
    assert_eq!(filter.radius.keyframes()[0].0, 105);
    assert_eq!(filter.direction.keyframes()[0].0, 105);
}

#[test]
fn pasted_color_corrections_bring_their_keyframes_into_the_target_clip() {
    let mut source = source_clip();
    let mut filter = vv_core::ClipFilter::new(vv_core::FilterKind::ColorCorrection);
    filter
        .grade
        .track_mut(vv_core::GradeParam::HighlightsY)
        .upsert(5, 0.4, Interpolation::Linear);
    source.effects.filters.push(filter);
    let target = clip(2, 100, 120);

    let pasted = merged_attributes(
        &source,
        &target,
        &HashSet::from([Attribute::Filters]),
        KeyframeMode::MaintainTiming,
    );

    let track = pasted.effects.filters[0]
        .grade
        .track(vv_core::GradeParam::HighlightsY);
    assert_eq!(track.keyframes()[0].0, 105);
}
