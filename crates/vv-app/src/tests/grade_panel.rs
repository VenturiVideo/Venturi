use super::*;

#[test]
fn the_wheels_go_one_two_or_four_per_row() {
    let width = |columns: f32| columns * WHEEL_BLOCK_WIDTH + (columns - 1.0) * 8.0;
    assert_eq!(wheel_columns(width(1.0) - 10.0, 8.0), 1);
    assert_eq!(wheel_columns(width(1.0), 8.0), 1);
    assert_eq!(wheel_columns(width(2.0), 8.0), 2);
    assert_eq!(wheel_columns(width(3.0), 8.0), 2, "never three and one");
    assert_eq!(wheel_columns(width(4.0), 8.0), 4);
}

#[test]
fn a_wheel_keyframe_row_follows_any_of_its_params() {
    let key = |on_keyframe, prev, next| RowKeyframe {
        on_keyframe,
        prev,
        next,
    };
    let joint = joint_keyframe([
        key(false, Some(3), None),
        key(true, Some(7), Some(20)),
        key(false, None, Some(12)),
    ]);
    assert!(joint.on_keyframe);
    assert_eq!(joint.prev, Some(7));
    assert_eq!(joint.next, Some(12));
}

fn project_with_clips(
    graded: &[bool],
) -> (
    vv_core::Project,
    vv_core::TimelineId,
    Vec<crate::PanelTarget>,
) {
    let mut project = vv_core::Project::default();
    let mut track = vv_core::Track::new(vv_core::TrackKind::Video);
    for (i, &graded) in graded.iter().enumerate() {
        let mut clip = vv_core::Clip::from_source_range(
            vv_core::ClipId(i as u64),
            vv_core::ClipSource::SolidColor,
            0,
            10,
            i as FrameIdx * 10,
            vv_core::Rational::one(),
        );
        if graded {
            clip.effects.filters.push(vv_core::ClipFilter::new(
                vv_core::FilterKind::ColorCorrection,
            ));
        }
        track.clips.push(clip);
    }
    let timeline = project.timelines.insert(vv_core::Timeline {
        name: "T".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (64, 36),
        tracks: vec![track],
        markers: Vec::new(),
        master: Default::default(),
    });
    let targets = (0..graded.len())
        .map(|i| crate::PanelTarget {
            timeline,
            track_index: 0,
            clip_id: vv_core::ClipId(i as u64),
            source_frame: 4,
            timeline_start: i as FrameIdx * 10,
            is_solid_color: true,
            is_text: false,
        })
        .collect();
    (project, timeline, targets)
}

fn apply(project: &mut vv_core::Project, commands: Vec<crate::properties_panel::BoxedCommand>) {
    let mut history = vv_core::History::default();
    for command in commands {
        history.do_command(project, command);
    }
}

fn grade_of(
    project: &vv_core::Project,
    timeline: vv_core::TimelineId,
    clip: u64,
) -> &vv_core::GradeTracks {
    let clip = project.timelines[timeline]
        .clip(0, vv_core::ClipId(clip))
        .unwrap();
    &clip
        .effects
        .filters
        .iter()
        .find(|f| f.kind.has_grade())
        .unwrap()
        .grade
}

fn edits(edits: &[(KeyframeEdit, GradeParam)]) -> GradeSectionResponse {
    GradeSectionResponse {
        keyframes: edits
            .iter()
            .map(|(edit, param)| (*edit, KeyframeTarget::Grade(*param)))
            .collect(),
        ..Default::default()
    }
}

#[test]
fn a_grade_edit_sets_the_default_of_every_graded_target() {
    let (mut project, timeline, targets) = project_with_clips(&[true, false, true]);
    let section = edits(&[(
        KeyframeEdit::Set(KeyframeValue::Grade(GradeParam::MidtonesLuma, 0.3)),
        GradeParam::MidtonesLuma,
    )]);
    let commands = grade_commands(Some(&project.timelines[timeline]), &targets, &section);
    apply(&mut project, commands);
    for clip in [0, 2] {
        let track = grade_of(&project, timeline, clip).track(GradeParam::MidtonesLuma);
        assert!(track.is_constant());
        assert_eq!(track.default, 0.3);
    }
}

#[test]
fn a_grade_edit_on_an_animated_param_sets_a_keyframe() {
    let (mut project, timeline, targets) = project_with_clips(&[true]);
    let toggle = edits(&[(KeyframeEdit::Toggle(false), GradeParam::ShadowsX)]);
    let commands = grade_commands(Some(&project.timelines[timeline]), &targets, &toggle);
    apply(&mut project, commands);
    let set = edits(&[(
        KeyframeEdit::Set(KeyframeValue::Grade(GradeParam::ShadowsX, 0.5)),
        GradeParam::ShadowsX,
    )]);
    let commands = grade_commands(Some(&project.timelines[timeline]), &targets, &set);
    apply(&mut project, commands);
    let track = grade_of(&project, timeline, 0).track(GradeParam::ShadowsX);
    assert_eq!(track.keyframe_at(4).map(|k| k.0), Some(0.5));
}

#[test]
fn a_wheel_with_one_animated_param_keyframes_the_others() {
    let (mut project, timeline, targets) = project_with_clips(&[true]);
    let toggle = edits(&[(KeyframeEdit::Toggle(false), GradeParam::ShadowsX)]);
    let commands = grade_commands(Some(&project.timelines[timeline]), &targets, &toggle);
    apply(&mut project, commands);
    let set = edits(&[(
        KeyframeEdit::Set(KeyframeValue::Grade(GradeParam::ShadowsY, 0.5)),
        GradeParam::ShadowsY,
    )]);
    let commands = grade_commands(Some(&project.timelines[timeline]), &targets, &set);
    apply(&mut project, commands);
    let track = grade_of(&project, timeline, 0).track(GradeParam::ShadowsY);
    assert_eq!(track.keyframe_at(4).map(|k| k.0), Some(0.5));
    assert_eq!(
        track.default,
        GradeParam::ShadowsY.neutral(),
        "the rest of the clip keeps its value"
    );
}

#[test]
fn removing_a_wheel_keyframe_skips_the_params_without_one() {
    let (mut project, timeline, targets) = project_with_clips(&[true]);
    let toggle = edits(&[(KeyframeEdit::Toggle(false), GradeParam::ShadowsX)]);
    let commands = grade_commands(Some(&project.timelines[timeline]), &targets, &toggle);
    apply(&mut project, commands);
    let remove = edits(&[
        (KeyframeEdit::Toggle(true), GradeParam::ShadowsX),
        (KeyframeEdit::Toggle(true), GradeParam::ShadowsY),
    ]);
    let commands = grade_commands(Some(&project.timelines[timeline]), &targets, &remove);
    assert_eq!(commands.len(), 1);
}

#[test]
fn a_preset_and_a_reset_replace_the_grade() {
    let (mut project, timeline, targets) = project_with_clips(&[true]);
    let section = GradeSectionResponse {
        preset: Some(vv_core::GradePreset::BlackAndWhite),
        ..Default::default()
    };
    let commands = grade_commands(Some(&project.timelines[timeline]), &targets, &section);
    apply(&mut project, commands);
    assert_eq!(
        grade_of(&project, timeline, 0).value_at(0),
        vv_core::GradePreset::BlackAndWhite.value()
    );
    let section = GradeSectionResponse {
        reset: vec![GradeParam::Saturation],
        ..Default::default()
    };
    let commands = grade_commands(Some(&project.timelines[timeline]), &targets, &section);
    apply(&mut project, commands);
    assert_eq!(
        grade_of(&project, timeline, 0).value_at(0),
        GradeValue::NEUTRAL
    );
}

#[test]
fn adding_a_grade_skips_the_clips_that_have_one() {
    let (mut project, timeline, targets) = project_with_clips(&[true, false]);
    let commands = add_grade_commands(Some(&project.timelines[timeline]), &targets);
    assert_eq!(commands.len(), 1);
    apply(&mut project, commands);
    assert_eq!(
        grade_of(&project, timeline, 1).value_at(0),
        GradeValue::NEUTRAL
    );
}

#[test]
fn a_balance_edits_only_the_wheels_it_moves() {
    let cast = [120u8, 128, 150, 255];
    let pixels: Vec<u8> = std::iter::repeat_n(cast, 64).flatten().collect();
    let mut section = GradeSectionResponse::default();
    balance_edits(&mut section, &GradeValue::NEUTRAL, &pixels);
    let params: Vec<GradeParam> = section
        .keyframes
        .iter()
        .filter_map(|(edit, _)| match edit {
            KeyframeEdit::Set(KeyframeValue::Grade(param, _)) => Some(*param),
            _ => None,
        })
        .collect();
    assert!(params.contains(&GradeParam::MidtonesX));
    assert!(params.contains(&GradeParam::MidtonesY));
    assert!(
        params
            .iter()
            .all(|p| ![GradeParam::OffsetX, GradeParam::Saturation].contains(p))
    );
}
