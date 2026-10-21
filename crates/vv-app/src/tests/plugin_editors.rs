use vv_core::{AudioEffect, Rational, Timeline, Track, TrackKind};

use super::*;

fn plugin_effect(state: &[u8]) -> Vst3Effect {
    Vst3Effect {
        id: EffectId(7),
        name: "Comp".into(),
        ..vv_plugin::testing::fixture_effect_of("ab", state)
    }
}

fn setup() -> (Project, TimelineId, Key) {
    let mut project = Project::default();
    let mut timeline = Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (64, 36),
        tracks: vec![Track::new(TrackKind::Audio)],
        markers: Vec::new(),
        master: Default::default(),
    };
    let mut off = AudioEffect::new(AudioEffectKind::Vst3(plugin_effect(&[1])));
    off.enabled = false;
    timeline.master.effects = vec![
        AudioEffect::new(AudioEffectKind::Mono),
        AudioEffect::new(AudioEffectKind::Vst3(plugin_effect(&[1]))),
        off,
    ];
    let id = project.timelines.insert(timeline);
    (project, id, (id, Group::Master, EffectId(7), 0))
}

fn state_of(project: &Project, id: TimelineId, index: usize) -> Vec<u8> {
    match &project.timelines[id].master.effects[index].kind {
        AudioEffectKind::Vst3(v) => v.component_state.bytes().to_vec(),
        _ => panic!("not a plugin"),
    }
}

#[test]
fn plugin_gesture_becomes_one_undoable_command() {
    let (mut project, id, key) = setup();
    let mut history = History::default();
    let hash = record_edit(
        &mut project,
        &mut history,
        key,
        PluginState::new(vec![2, 2]),
        PluginState::new(vec![3]),
    );
    // Found by effect id even though it is not the first plugin of the chain.
    assert_eq!(state_of(&project, id, 1), [2, 2]);
    let AudioEffectKind::Vst3(new) = &project.timelines[id].master.effects[1].kind else {
        panic!("not a plugin");
    };
    assert_eq!(hash, Some(new.content_hash()));
    assert_eq!(history.labels().count(), 1);
    assert!(
        project.timelines[id].master.effects[1].enabled,
        "the edit keeps the effect's other fields"
    );

    history.undo(&mut project);
    assert_eq!(state_of(&project, id, 1), [1]);
    history.redo(&mut project);
    assert_eq!(state_of(&project, id, 1), [2, 2]);
}

#[test]
fn a_gesture_that_changed_nothing_leaves_no_undo_step() {
    let (mut project, _, key) = setup();
    let mut history = History::default();
    let same = record_edit(
        &mut project,
        &mut history,
        key,
        PluginState::new(vec![1]),
        PluginState::new(Vec::new()),
    );
    assert_eq!(same, None);
    assert_eq!(history.labels().count(), 0);
}

#[test]
fn a_gesture_of_a_removed_effect_is_dropped() {
    let (mut project, id, _) = setup();
    let mut history = History::default();
    let gone = (id, Group::Master, EffectId(99), 0);
    let state = PluginState::new(vec![5]);
    assert_eq!(
        record_edit(
            &mut project,
            &mut history,
            gone,
            state,
            PluginState::default()
        ),
        None
    );
    assert_eq!(history.labels().count(), 0);
}

#[derive(Default)]
struct Fake {
    gesture: std::sync::Mutex<Option<(PluginState, PluginState)>>,
    hash: std::sync::atomic::AtomicU64,
    serviced: std::sync::atomic::AtomicUsize,
    seq: std::sync::atomic::AtomicU64,
}

impl Fake {
    fn with_gesture(component: Vec<u8>) -> Self {
        let fake = Fake::default();
        *fake.gesture.lock().unwrap() =
            Some((PluginState::new(component), PluginState::new(Vec::new())));
        fake
    }

    fn at(self, seq: u64) -> Self {
        self.seq.store(seq, std::sync::atomic::Ordering::Relaxed);
        self
    }
}

impl vv_audio::PluginProcessor for Fake {
    fn process(&self, _: &mut [f32], _: usize) -> bool {
        true
    }
    fn reset(&self) {}
    fn latency(&self) -> u32 {
        0
    }
}

impl HostedPlugin for Fake {
    fn set_state(&self, _: &Vst3Effect) {}
    fn state_hash(&self) -> u64 {
        self.hash.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn mark_state(&self, hash: u64) {
        self.hash.store(hash, std::sync::atomic::Ordering::Relaxed);
    }
    fn service_restart(&self) {
        self.serviced
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn take_gesture(&self) -> Option<(PluginState, PluginState)> {
        self.gesture.lock().unwrap().take()
    }
    fn gesture_seq(&self) -> u64 {
        self.seq.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[test]
fn only_the_editors_of_dropped_instances_close_on_a_rescan() {
    let (kept, gone) = (Arc::new(Fake::default()), Arc::new(Fake::default()));
    let dropped: Vec<Arc<dyn HostedPlugin>> = vec![gone.clone()];
    assert!(is_dropped(&dropped, &gone));
    assert!(!is_dropped(&dropped, &kept));
    assert!(!is_dropped(&[], &gone));
}

#[test]
fn an_editor_instance_is_held_under_the_key_the_mix_asks() {
    let (mut project, id, key) = setup();
    let instance: Arc<dyn HostedPlugin> = Arc::new(Fake::default());
    let held = held_instances(&project, [(key, instance.clone())].into_iter());
    assert_eq!(held.len(), 1);
    let expected = plugin_key_of(&project.timelines[id], MixerChannel::Master, 1);
    assert_eq!(held[0].0, expected);
    assert!(Arc::ptr_eq(&held[0].1, &instance));

    // Disabled, the key is the same one the enabled effect plays under.
    project.timelines[id].master.effects[1].enabled = false;
    let again = held_instances(&project, [(key, instance)].into_iter());
    assert_eq!(again[0].0, held[0].0);

    let gone = (id, Group::Master, EffectId(99), 0);
    assert!(
        held_instances(
            &project,
            [(gone, Arc::new(Fake::default()) as _)].into_iter()
        )
        .is_empty()
    );
}

#[test]
fn a_gesture_pending_at_close_is_recorded_once() {
    let (mut project, id, key) = setup();
    let mut history = History::default();
    let fake = Fake::with_gesture(vec![9]);
    record_gesture(&mut project, &mut history, key, &fake, false);
    assert_eq!(state_of(&project, id, 1), [9]);
    assert_eq!(history.labels().count(), 1);
    assert_ne!(fake.state_hash(), 0, "the pool must not reload the state");

    record_gesture(&mut project, &mut history, key, &fake, false);
    assert_eq!(history.labels().count(), 1);
}

#[test]
fn the_gesture_of_a_removed_effect_is_consumed_not_recorded() {
    let (mut project, id, _) = setup();
    let mut history = History::default();
    let gone = (id, Group::Master, EffectId(99), 0);
    let fake = Fake::with_gesture(vec![9]);
    record_gesture(&mut project, &mut history, gone, &fake, false);
    assert_eq!(history.labels().count(), 0);
    assert!(fake.take_gesture().is_none(), "nothing left for a reopen");
}

#[test]
fn a_gesture_during_an_undo_group_is_its_own_step_after_it() {
    let (mut project, id, key) = setup();
    let mut history = History::default();
    let fake = Fake::with_gesture(vec![9]);

    let mark = history.begin_group();
    assert!(!record_gesture(
        &mut project,
        &mut history,
        key,
        &fake,
        true
    ));
    assert_eq!(history.labels().count(), 0);
    assert_eq!(state_of(&project, id, 1), [1]);
    history.end_group(mark);

    assert!(record_gesture(
        &mut project,
        &mut history,
        key,
        &fake,
        false
    ));
    assert_eq!(state_of(&project, id, 1), [9]);
    assert_eq!(history.labels().count(), 1);
    history.undo(&mut project);
    assert_eq!(state_of(&project, id, 1), [1]);
}

#[test]
fn a_closed_editors_gesture_waits_for_the_group_then_is_its_own_step() {
    let (mut project, id, key) = setup();
    let mut history = History::default();
    let fake: Arc<dyn HostedPlugin> = Arc::new(Fake::with_gesture(vec![9]));
    let mut pending = vec![(key, fake)];

    let mark = history.begin_group();
    drain_pending(&mut pending, &mut project, &mut history, true);
    assert_eq!(pending.len(), 1);
    assert_eq!(history.labels().count(), 0);
    history.end_group(mark);

    drain_pending(&mut pending, &mut project, &mut history, false);
    assert!(pending.is_empty());
    assert_eq!(state_of(&project, id, 1), [9]);
    assert_eq!(history.labels().count(), 1);
    history.undo(&mut project);
    assert_eq!(state_of(&project, id, 1), [1]);
}

#[test]
fn clicking_an_open_editor_raises_it_instead_of_doing_nothing() {
    let (_, _, key) = setup();
    let raised = std::cell::Cell::new(0);
    let mut windows = HashMap::from([(key, true)]);
    assert!(raise_open(
        &mut windows,
        &key,
        |open| *open,
        |_| raised.set(raised.get() + 1)
    ));
    assert_eq!(raised.get(), 1);
}

#[test]
fn a_closed_or_missing_editor_is_not_raised() {
    let (_, _, key) = setup();
    let raised = std::cell::Cell::new(0);
    let mut windows = HashMap::from([(key, false)]);
    assert!(!raise_open(
        &mut windows,
        &key,
        |open| *open,
        |_| raised.set(1)
    ));
    windows.clear();
    assert!(!raise_open(
        &mut windows,
        &key,
        |open| *open,
        |_| raised.set(1)
    ));
    assert_eq!(raised.get(), 0);
}

#[test]
fn an_open_editor_instance_is_serviced_for_restarts_even_in_an_undo_group() {
    let (mut project, _, key) = setup();
    let mut history = History::default();
    let fake = Fake::default();
    record_gesture(&mut project, &mut history, key, &fake, false);
    record_gesture(&mut project, &mut history, key, &fake, true);
    assert_eq!(fake.serviced.load(std::sync::atomic::Ordering::Relaxed), 2);
}

/// Master strip `[X(off), X]`: the same id twice, the first disabled.
fn duplicate_strip() -> (Project, TimelineId) {
    let (mut project, id, _) = setup();
    let mut off = AudioEffect::new(AudioEffectKind::Vst3(plugin_effect(&[10])));
    off.enabled = false;
    let on = AudioEffect::new(AudioEffectKind::Vst3(plugin_effect(&[20])));
    project.timelines[id].master.effects = vec![off, on];
    (project, id)
}

#[test]
fn duplicate_ids_on_a_strip_get_distinct_editor_keys() {
    let (project, id) = duplicate_strip();
    let key = |index| key_at(&project.timelines[id], id, MixerChannel::Master, index);
    assert_eq!(key(0), Some((id, Group::Master, EffectId(7), 0)));
    assert_eq!(key(1), Some((id, Group::Master, EffectId(7), 1)));
}

#[test]
fn a_gesture_is_recorded_into_the_duplicate_whose_editor_is_open() {
    let (mut project, id) = duplicate_strip();
    let mut history = History::default();
    let second = (id, Group::Master, EffectId(7), 1);
    record_edit(
        &mut project,
        &mut history,
        second,
        PluginState::new(vec![9]),
        PluginState::default(),
    );
    assert_eq!(state_of(&project, id, 0), [10]);
    assert_eq!(state_of(&project, id, 1), [9]);
    let first = (id, Group::Master, EffectId(7), 0);
    record_edit(
        &mut project,
        &mut history,
        first,
        PluginState::new(vec![8]),
        PluginState::default(),
    );
    assert_eq!(state_of(&project, id, 0), [8]);
    assert_eq!(state_of(&project, id, 1), [9]);
}

#[test]
fn each_duplicate_editor_is_held_under_its_own_mix_key() {
    let (project, id) = duplicate_strip();
    let instance: Arc<dyn HostedPlugin> = Arc::new(Fake::default());
    for index in 0..2 {
        let key = (id, Group::Master, EffectId(7), index as u32);
        let held = held_instances(&project, [(key, instance.clone())].into_iter());
        let expected = plugin_key_of(&project.timelines[id], MixerChannel::Master, index);
        assert_eq!(held[0].0, expected);
    }
}

/// Tracks `[V1, A1, A2(X)]`; the plugin sits on the last one.
fn tracks_with_plugin() -> (Project, TimelineId) {
    let (mut project, id, _) = setup();
    let t = &mut project.timelines[id];
    t.tracks = vec![
        Track::new(TrackKind::Video),
        Track::new(TrackKind::Audio),
        Track::new(TrackKind::Audio),
    ];
    t.tracks[2].mix.effects = vec![AudioEffect::new(AudioEffectKind::Vst3(plugin_effect(&[1])))];
    (project, id)
}

fn editor_key(project: &Project, id: TimelineId, track: usize) -> Key {
    let channel = MixerChannel::Track(track);
    key_at(&project.timelines[id], id, channel, 0).unwrap()
}

#[test]
fn an_editor_key_survives_the_removal_of_an_earlier_track() {
    let (mut project, id) = tracks_with_plugin();
    let key = editor_key(&project, id, 2);
    let mut history = History::default();

    project.timelines[id].tracks.remove(0);
    let (channel, index, _) = find(&project, key).expect("the effect still exists");
    assert_eq!((channel, index), (MixerChannel::Track(1), 0));

    let fake = Fake::with_gesture(vec![9]);
    assert!(record_gesture(
        &mut project,
        &mut history,
        key,
        &fake,
        false
    ));
    assert_eq!(history.labels().count(), 1);
    let AudioEffectKind::Vst3(v) = &project.timelines[id].tracks[1].mix.effects[0].kind else {
        panic!("not a plugin");
    };
    assert_eq!(v.component_state.bytes(), [9]);
}

#[test]
fn an_editor_key_still_finds_its_effect_when_a_track_comes_back() {
    let (mut project, id) = tracks_with_plugin();
    let key = editor_key(&project, id, 2);
    let removed = project.timelines[id].tracks.remove(1);
    assert!(find(&project, key).is_some());
    project.timelines[id].tracks.insert(0, removed);
    let (channel, _, _) = find(&project, key).expect("found at its new place");
    assert_eq!(channel, MixerChannel::Track(2));
}

#[test]
fn a_held_editor_follows_the_mix_key_after_a_track_removal() {
    let (mut project, id) = tracks_with_plugin();
    let key = editor_key(&project, id, 2);
    let instance: Arc<dyn HostedPlugin> = Arc::new(Fake::default());
    let before = held_instances(&project, [(key, instance.clone())].into_iter());
    project.timelines[id].tracks.remove(0);
    let after = held_instances(&project, [(key, instance)].into_iter());
    assert_eq!(after.len(), 1);
    assert_eq!(
        after[0].0, before[0].0,
        "the mixer's key omits the track index"
    );
}

#[test]
fn the_same_plugin_on_two_tracks_gets_distinct_editor_keys() {
    let (mut project, id) = tracks_with_plugin();
    project.timelines[id].tracks[1].mix.effects =
        vec![AudioEffect::new(AudioEffectKind::Vst3(plugin_effect(&[5])))];
    let first = editor_key(&project, id, 1);
    let second = editor_key(&project, id, 2);
    assert_ne!(first, second);
    assert_eq!(find(&project, first).unwrap().0, MixerChannel::Track(1));
    assert_eq!(find(&project, second).unwrap().0, MixerChannel::Track(2));
}

#[test]
fn a_removed_track_with_the_effect_drops_its_gesture() {
    let (mut project, id) = tracks_with_plugin();
    let key = editor_key(&project, id, 2);
    let mut history = History::default();
    project.timelines[id].tracks.truncate(2);
    let fake = Fake::with_gesture(vec![9]);
    record_gesture(&mut project, &mut history, key, &fake, false);
    assert_eq!(history.labels().count(), 0);
    assert!(fake.take_gesture().is_none());
}

fn two_strips() -> (Project, TimelineId, [Key; 2]) {
    let (mut project, id, _) = setup();
    let mut second = AudioEffect::new(AudioEffectKind::Vst3(plugin_effect(&[1])));
    if let AudioEffectKind::Vst3(v) = &mut second.kind {
        v.id = EffectId(8);
    }
    project.timelines[id].master.effects.push(second);
    let keys = [
        (id, Group::Master, EffectId(7), 0),
        (id, Group::Master, EffectId(8), 0),
    ];
    (project, id, keys)
}

fn effect_order(history: &History) -> usize {
    history.labels().count()
}

#[test]
fn pending_gestures_are_recorded_in_the_order_they_ended() {
    for order in [[0, 1], [1, 0]] {
        let (mut project, id, keys) = two_strips();
        let mut history = History::default();
        // The editor of keys[order[0]] ended its gesture first.
        let mut pending: Vec<(Key, Arc<dyn HostedPlugin>)> = Vec::new();
        for (rank, which) in order.iter().enumerate() {
            let fake = Fake::with_gesture(vec![10 + rank as u8]).at(1 + rank as u64);
            pending.push((keys[*which], Arc::new(fake)));
        }
        // Whatever order they were queued in, the earlier gesture goes first.
        pending.reverse();
        drain_pending(&mut pending, &mut project, &mut history, false);
        assert_eq!(effect_order(&history), 2);
        history.undo(&mut project);
        // The later gesture was the last step, so only it is undone.
        let later = keys[order[1]];
        let (_, index, _) = find(&project, later).unwrap();
        assert_eq!(state_of(&project, id, index), [1], "order {order:?}");
        let earlier = keys[order[0]];
        let (_, index, _) = find(&project, earlier).unwrap();
        assert_eq!(state_of(&project, id, index), [10], "order {order:?}");
    }
}

#[test]
fn closing_in_an_open_group_defers_and_records_after_it() {
    let (mut project, id, key) = setup();
    let mut history = History::default();
    let mut editors = PluginEditors::default();
    editors.defer_gesture(key, Arc::new(Fake::with_gesture(vec![9])));
    let mark = history.begin_group();
    editors.close_instances(&mut project, &mut history, &[], true);
    assert_eq!(state_of(&project, id, 1), [1]);
    history.end_group(mark);
    editors.close_instances(&mut project, &mut history, &[], false);
    assert_eq!(state_of(&project, id, 1), [9]);
    assert_eq!(history.labels().count(), 1);
}

#[test]
fn a_gesture_shares_the_half_it_did_not_change() {
    let (mut project, id, key) = setup();
    let before = match &project.timelines[id].master.effects[1].kind {
        AudioEffectKind::Vst3(v) => v.clone(),
        _ => panic!("not a plugin"),
    };
    let mut history = History::default();
    record_edit(
        &mut project,
        &mut history,
        key,
        PluginState::new(before.component_state.bytes().to_vec()),
        PluginState::new(vec![9, 9]),
    );
    let AudioEffectKind::Vst3(after) = &project.timelines[id].master.effects[1].kind else {
        panic!("not a plugin");
    };
    assert_eq!(after.controller_state.bytes(), [9, 9]);
    assert!(
        after
            .component_state
            .shares_bytes_with(&before.component_state)
    );
}

#[test]
fn the_aside_key_is_the_placement_key_under_the_editor_scope() {
    let (mut project, id, _) = setup();
    let other = project.timelines.insert(project.timelines[id].clone());
    let key = |timeline, index| {
        aside_key(
            timeline,
            &project.timelines[timeline],
            MixerChannel::Master,
            index,
        )
    };

    let live = plugin_key_of(&project.timelines[id], MixerChannel::Master, 1);
    assert_ne!(key(id, 1), live);
    assert_ne!(key(id, 1), key(id, 2), "a placement each");
    assert_ne!(key(id, 1), key(other, 1), "a timeline each");
    assert_eq!(key(id, 1), key(id, 1));
    assert_eq!(
        key(id, 1),
        vv_audio::plugin_key_in(
            vv_core::stable_hash(("editor", id.raw())),
            &project.timelines[id],
            MixerChannel::Master,
            1
        )
        .unwrap()
    );
}
