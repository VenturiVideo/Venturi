use super::*;
use vv_core::{
    Clip, ClipId, ClipSource, FilterKind, History, Project, Rational, Timeline, Track, TrackKind,
};

#[test]
fn panel_edits_and_keyframes_form_an_undoable_step() {
    let mut project = Project::default();
    let mut clip = Clip::from_source_range(
        ClipId(0),
        ClipSource::SolidColor,
        0,
        100,
        0,
        Rational::one(),
    );
    clip.effects
        .filters
        .push(vv_core::ClipFilter::new(FilterKind::CameraShake));
    let mut track = Track::new(TrackKind::Video);
    track.clips.push(clip);
    let timeline = project.timelines.insert(Timeline {
        name: "Shake".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![track],
        markers: Vec::new(),
        master: Default::default(),
    });
    let clip_ref = (timeline, 0, ClipId(0));
    let original = project.timelines[timeline].tracks[0].clips[0]
        .effects
        .filters
        .clone();
    let response = ShakeResponse {
        edits: vec![
            (
                ShakeParam::Pan,
                KeyframeEdit::Set(KeyframeValue::CameraShake(ShakeParam::Pan, 2.0)),
            ),
            (ShakeParam::Pan, KeyframeEdit::Toggle(false)),
        ],
        seed: Some(123),
        auto_zoom: Some(false),
        ..Default::default()
    };
    let mut filters = original.clone();
    let mut keyframes = response.commands(clip_ref, 12, &mut filters[0].shake);
    let mut commands: Vec<BoxedCommand> = vec![Box::new(vv_core::set_clip_filters(
        timeline,
        0,
        ClipId(0),
        filters,
    ))];
    commands.append(&mut keyframes);
    let mut history = History::default();
    history.do_command(
        &mut project,
        Box::new(vv_core::CompositeCommand::new(
            vv_core::CommandLabel::SetKeyframe,
            commands,
        )),
    );
    let shake = &project.timelines[timeline].tracks[0].clips[0]
        .effects
        .filters[0]
        .shake;
    assert_eq!(shake.seed, 123);
    assert!(!shake.auto_zoom);
    assert_eq!(shake.track(ShakeParam::Pan).keyframe_at(12).unwrap().0, 2.0);
    history.undo(&mut project);
    assert_eq!(
        project.timelines[timeline].tracks[0].clips[0]
            .effects
            .filters,
        original
    );
    history.redo(&mut project);
    assert_eq!(
        project.timelines[timeline].tracks[0].clips[0]
            .effects
            .filters[0]
            .shake
            .track(ShakeParam::Pan)
            .keyframe_at(12)
            .unwrap()
            .0,
        2.0
    );
}

#[test]
fn reset_removes_animation_and_restores_seed_and_edge_compensation() {
    let mut shake = CameraShakeTracks::default();
    shake.seed = 123;
    shake.auto_zoom = false;
    shake
        .track_mut(ShakeParam::MotionScale)
        .upsert(10, 2.0, vv_core::Interpolation::Linear);
    let mut project = Project::default();
    let timeline = project.timelines.insert(Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: Vec::new(),
        markers: Vec::new(),
        master: Default::default(),
    });
    let response = ShakeResponse {
        reset_all: true,
        ..Default::default()
    };
    assert!(
        response
            .commands((timeline, 0, ClipId(0)), 10, &mut shake)
            .is_empty()
    );
    assert_eq!(shake, CameraShakeTracks::default());
}
