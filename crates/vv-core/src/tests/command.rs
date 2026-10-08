use super::*;
use crate::{Rational, Timeline, Track};

fn project() -> (Project, TimelineId) {
    let mut project = Project::default();
    let timeline = project.timelines.insert(Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (64, 48),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    (project, timeline)
}

fn add_track(timeline: TimelineId) -> Box<dyn Command> {
    Box::new(AddTrack::new(timeline, TrackKind::Audio))
}

fn tracks(project: &Project, timeline: TimelineId) -> usize {
    project.timelines[timeline].tracks.len()
}

#[test]
fn joined_commands_are_one_step() {
    let (mut project, tl) = project();
    let mut history = History::default();
    let label = CommandLabel::RemoveMedia;

    let step = history.join(&mut project, None, label, add_track(tl));
    let step = history.join(&mut project, Some(step), label, add_track(tl));
    history.join(&mut project, Some(step), label, add_track(tl));

    assert_eq!(history.position(), 1);
    assert_eq!(history.labels().collect::<Vec<_>>(), vec![label]);
    history.undo(&mut project);
    assert_eq!(tracks(&project, tl), 1);
    history.redo(&mut project);
    assert_eq!(tracks(&project, tl), 4);
}

#[test]
fn each_join_is_a_change() {
    let (mut project, tl) = project();
    let mut history = History::default();

    let step = history.join(&mut project, None, CommandLabel::AddTrack, add_track(tl));
    let generation = history.generation();
    history.join(
        &mut project,
        Some(step),
        CommandLabel::AddTrack,
        add_track(tl),
    );

    assert!(history.generation() > generation);
}

#[test]
fn a_command_in_between_starts_a_new_step() {
    let (mut project, tl) = project();
    let mut history = History::default();

    let step = history.join(&mut project, None, CommandLabel::RemoveMedia, add_track(tl));
    history.do_command(&mut project, add_track(tl));
    history.join(
        &mut project,
        Some(step),
        CommandLabel::RemoveMedia,
        add_track(tl),
    );

    assert_eq!(history.position(), 3);
}

#[test]
fn an_undo_in_between_starts_a_new_step_and_drops_the_redo() {
    let (mut project, tl) = project();
    let mut history = History::default();

    let step = history.join(&mut project, None, CommandLabel::RemoveMedia, add_track(tl));
    history.undo(&mut project);
    history.join(
        &mut project,
        Some(step),
        CommandLabel::RemoveMedia,
        add_track(tl),
    );

    assert_eq!(history.position(), 1);
    assert_eq!(history.labels().count(), 1);
    assert_eq!(tracks(&project, tl), 2);
}

#[test]
fn a_group_closed_around_the_step_ends_it() {
    let (mut project, tl) = project();
    let mut history = History::default();

    let mark = history.begin_group();
    history.do_command(&mut project, add_track(tl));
    let step = history.join(&mut project, None, CommandLabel::RemoveMedia, add_track(tl));
    history.end_group_as(mark, CommandLabel::Gain);
    history.join(
        &mut project,
        Some(step),
        CommandLabel::RemoveMedia,
        add_track(tl),
    );

    assert_eq!(
        history.labels().collect::<Vec<_>>(),
        vec![CommandLabel::Gain, CommandLabel::RemoveMedia]
    );
    history.undo(&mut project);
    assert_eq!(tracks(&project, tl), 3);
}

#[test]
fn only_the_latest_handle_joins() {
    let (mut project, tl) = project();
    let mut history = History::default();

    let first = history.join(&mut project, None, CommandLabel::RemoveMedia, add_track(tl));
    history.join(
        &mut project,
        Some(first),
        CommandLabel::RemoveMedia,
        add_track(tl),
    );
    history.join(
        &mut project,
        Some(first),
        CommandLabel::RemoveMedia,
        add_track(tl),
    );

    assert_eq!(history.position(), 2);
}

#[test]
fn a_joinable_step_never_absorbs_a_plain_composite() {
    let (mut project, tl) = project();
    let mut history = History::default();

    let step = history.join(&mut project, None, CommandLabel::RemoveMedia, add_track(tl));
    history.undo(&mut project);
    history.do_command(
        &mut project,
        Box::new(CompositeCommand::new(
            CommandLabel::Gain,
            vec![add_track(tl)],
        )),
    );
    history.join(
        &mut project,
        Some(step),
        CommandLabel::RemoveMedia,
        add_track(tl),
    );

    assert_eq!(history.position(), 2);
}

#[test]
fn an_explicit_label_renames_a_group_of_one_step() {
    let (mut project, tl) = project();
    let mut history = History::default();

    let mark = history.begin_group();
    history.do_command(&mut project, add_track(tl));
    history.end_group_as(mark, CommandLabel::InsertClips);

    assert_eq!(
        history.labels().collect::<Vec<_>>(),
        vec![CommandLabel::InsertClips]
    );
    history.undo(&mut project);
    assert_eq!(tracks(&project, tl), 1);
}

#[test]
fn mixer_params_set_track_and_master_and_undo() {
    let (mut project, tl) = project();
    let mut history = History::default();
    let set = |channel, param, value| Box::new(SetMixerParam::new(tl, channel, param, value));
    history.do_command(
        &mut project,
        set(MixerChannel::Track(0), MixerParam::GainDb, -6.0),
    );
    history.do_command(
        &mut project,
        set(MixerChannel::Master, MixerParam::GainDb, 1000.0),
    );
    history.do_command(
        &mut project,
        set(MixerChannel::Track(0), MixerParam::Pan, -3.0),
    );

    let timeline = &project.timelines[tl];
    assert_eq!(timeline.tracks[0].mix.gain_db, -6.0);
    assert_eq!(timeline.master.gain_db, crate::GAIN_DB_MAX, "clamped");
    assert_eq!(timeline.tracks[0].mix.pan, -1.0, "clamped");

    for _ in 0..3 {
        history.undo(&mut project);
    }
    let timeline = &project.timelines[tl];
    assert_eq!(timeline.tracks[0].mix, ChannelStrip::default());
    assert_eq!(timeline.master, ChannelStrip::default());
}

#[test]
fn a_track_saved_without_mixer_settings_loads_at_unity_gain() {
    let mut value = serde_json::to_value(Track::new(TrackKind::Audio)).unwrap();
    value.as_object_mut().unwrap().remove("mix");
    let track: Track = serde_json::from_value(value).unwrap();
    assert_eq!(track.mix, ChannelStrip::default());
}

#[test]
fn audio_effects_are_added_edited_and_removed_with_undo() {
    let (mut project, tl) = project();
    let mut history = History::default();
    let channel = MixerChannel::Master;
    let normalize = |target_db| {
        AudioEffect::new(crate::AudioEffectKind::Normalize {
            target_db,
            mode: Default::default(),
            set_level: Default::default(),
        })
    };
    let effects = |project: &Project| project.timelines[tl].master.effects.clone();

    history.do_command(
        &mut project,
        Box::new(AddAudioEffect::new(tl, channel, normalize(-1.0))),
    );
    history.do_command(
        &mut project,
        Box::new(AddAudioEffect::new(tl, channel, normalize(-3.0))),
    );
    let off = AudioEffect {
        enabled: false,
        ..normalize(-1.0)
    };
    let label = CommandLabel::ToggleAudioEffect;
    history.do_command(
        &mut project,
        Box::new(SetAudioEffect::new(tl, channel, 0, off.clone(), label)),
    );
    history.do_command(
        &mut project,
        Box::new(RemoveAudioEffect::new(tl, channel, 1)),
    );
    assert_eq!(effects(&project), [off]);
    assert_eq!(history.labels().nth(2), Some(label));

    history.undo(&mut project);
    history.undo(&mut project);
    assert_eq!(effects(&project), [normalize(-1.0), normalize(-3.0)]);
    history.undo(&mut project);
    history.undo(&mut project);
    assert!(effects(&project).is_empty());
}

#[test]
fn moving_an_audio_effect_reorders_the_chain_and_undoes() {
    let (mut project, tl) = project();
    let channel = MixerChannel::Track(0);
    let normalize = |target_db| {
        AudioEffect::new(crate::AudioEffectKind::Normalize {
            target_db,
            mode: Default::default(),
            set_level: Default::default(),
        })
    };
    project.timelines[tl].tracks[0].mix.effects =
        vec![normalize(-1.0), normalize(-2.0), normalize(-3.0)];
    let targets = |project: &Project| -> Vec<f32> {
        project.timelines[tl].tracks[0]
            .mix
            .effects
            .iter()
            .map(|e| match e.kind {
                crate::AudioEffectKind::Normalize { target_db, .. } => target_db,
                crate::AudioEffectKind::MultibandCompressor(_)
                | crate::AudioEffectKind::Mono
                | crate::AudioEffectKind::Equalizer(_) => f32::NAN,
            })
            .collect()
    };
    let mut history = History::default();

    history.do_command(
        &mut project,
        Box::new(MoveAudioEffect::new(tl, channel, 0, 2)),
    );
    assert_eq!(targets(&project), [-2.0, -3.0, -1.0]);
    history.do_command(
        &mut project,
        Box::new(MoveAudioEffect::new(tl, channel, 2, 1)),
    );
    assert_eq!(targets(&project), [-2.0, -1.0, -3.0]);
    history.undo(&mut project);
    history.undo(&mut project);
    assert_eq!(targets(&project), [-1.0, -2.0, -3.0]);
}

#[test]
fn the_processing_precision_is_an_undoable_step() {
    let (mut project, _) = project();
    let mut history = History::default();
    history.do_command(
        &mut project,
        Box::new(SetProcessingPrecision::new(ProcessingPrecision::Standard)),
    );
    assert_eq!(project.precision, ProcessingPrecision::Standard);
    history.undo(&mut project);
    assert_eq!(project.precision, ProcessingPrecision::High);
    history.redo(&mut project);
    assert_eq!(project.precision, ProcessingPrecision::Standard);
}
