use super::*;

fn zoom(param: TransformParam, frame: FrameIdx) -> KeyframePick {
    (KeyframeTarget::TransformParam(param), frame)
}

#[test]
fn zoom_link_adds_the_twin_axis_once_and_only_for_zoom() {
    let picks = vec![zoom(TransformParam::ZoomX, 5), (KeyframeTarget::Gain, 5)];
    let linked = with_zoom_link(picks.clone(), true);
    assert!(linked.contains(&zoom(TransformParam::ZoomY, 5)));
    assert_eq!(linked.len(), 3, "gain has no twins");

    let both = with_zoom_link(
        vec![
            zoom(TransformParam::ZoomX, 5),
            zoom(TransformParam::ZoomY, 5),
        ],
        true,
    );
    assert_eq!(both.len(), 2, "no duplicates if both are already selected");

    assert_eq!(with_zoom_link(picks.clone(), false), picks);
}

#[test]
fn the_delete_key_is_the_editors_only_with_a_selection_and_the_last_click() {
    let mut state = KeyframeEditorState::default();
    state.selection.insert((KeyframeTarget::Gain, 3));
    assert!(
        !state.owns_delete(),
        "without the last click Delete belongs to the timeline"
    );
    state.focused = true;
    assert!(state.owns_delete());
    state.selection.clear();
    assert!(
        !state.owns_delete(),
        "nothing to delete: Delete goes back to the clip"
    );
}

#[test]
fn removing_the_selection_empties_it_and_follows_the_zoom_link() {
    let mut state = KeyframeEditorState::default();
    state.clip = Some((<TimelineId as vv_core::Id>::from_raw(0), 0, ClipId(1)));
    state.selection.insert(zoom(TransformParam::ZoomX, 7));
    assert_eq!(state.remove_selected(true).len(), 2, "the Y twin too");
    assert!(state.selection.is_empty());

    let mut state = KeyframeEditorState::default();
    state.selection.insert((KeyframeTarget::Gain, 1));
    assert!(
        state.remove_selected(false).is_empty(),
        "without a clip there is nothing to remove"
    );
}

#[test]
fn the_curve_is_sampled_between_frames_not_only_on_them() {
    let keyframes = vec![
        (0, 0.0, Interpolation::Linear),
        (10, 100.0, Interpolation::Linear),
    ];
    assert_eq!(sample(&keyframes, 2.5), Some(25.0));
    assert_eq!(sample(&keyframes, -3.0), Some(0.0), "before the first");
    assert_eq!(sample(&keyframes, 40.0), Some(100.0), "after the last");
    assert_eq!(sample(&[], 1.0), None);
}

#[test]
fn a_hold_segment_stays_flat_until_the_next_keyframe() {
    let keyframes = vec![
        (0, 0.0, Interpolation::Hold),
        (10, 100.0, Interpolation::Linear),
    ];
    assert_eq!(sample(&keyframes, 9.9), Some(0.0));
    assert_eq!(sample(&keyframes, 10.0), Some(100.0));
}

#[test]
fn the_time_axis_maps_the_visible_window_onto_the_rect() {
    let rect = egui::Rect::from_min_size(egui::pos2(100.0, 0.0), egui::vec2(200.0, 10.0));
    let axis = TimeAxis::new(rect, (50, 100));
    assert_eq!(axis.x(50), 100.0);
    assert_eq!(axis.x(150), 300.0);
    assert_eq!(axis.frame(200.0), 100);
    assert_eq!(axis.delta(-20.0), -10);
}

#[test]
fn camera_shake_curves_are_exposed_to_the_keyframe_editor() {
    let mut effects = EffectStack::default();
    let mut filter = vv_core::ClipFilter::new(vv_core::FilterKind::CameraShake);
    filter
        .shake
        .track_mut(vv_core::ShakeParam::Pan)
        .upsert(10, 0.0, Interpolation::Linear);
    filter
        .shake
        .track_mut(vv_core::ShakeParam::Pan)
        .upsert(30, 2.0, Interpolation::EaseOut);
    effects.filters.push(filter);
    let target = KeyframeTarget::CameraShake(vv_core::ShakeParam::Pan);
    assert_eq!(rows(&effects), vec![target]);
    assert_eq!(frames_of(&effects, target), vec![10, 30]);
    assert_eq!(scalar_value_at(&effects, target, 20), Some(1.0));
    assert_eq!(
        scalar_at(&effects, target, 30),
        Some((2.0, Interpolation::EaseOut))
    );
    assert_eq!(scalar_keyframes(&effects, target).len(), 2);
}
