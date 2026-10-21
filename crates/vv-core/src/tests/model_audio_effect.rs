use super::*;

fn vst3(state: Vec<u8>) -> Vst3Effect {
    Vst3Effect {
        id: EffectId::fresh(),
        class_id: "0123456789ABCDEF0123456789ABCDEF".into(),
        name: "Test Plugin".into(),
        vendor: "Vendor".into(),
        version: "1.0".into(),
        component_state: PluginState::new(state),
        controller_state: PluginState::new(vec![7, 8, 9]),
    }
}

#[test]
fn vst3_effect_round_trips_through_ron() {
    let state: Vec<u8> = (0..70_000).map(|i| (i * 31 % 251) as u8).collect();
    let effect = AudioEffect::new(AudioEffectKind::Vst3(vst3(state.clone())));

    let mut track = Track::new(TrackKind::Audio);
    track.mix.effects.push(effect.clone());
    let mut project = Project::default();
    project.timelines.insert(Timeline {
        name: "Timeline 1".into(),
        fps: Rational::new(30, 1),
        resolution: (1920, 1080),
        tracks: vec![track],
        markers: Vec::new(),
        master: Default::default(),
    });

    let text = ron::ser::to_string(&project).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&state);
    assert!(text.contains(&encoded));

    let back: Project = ron::from_str(&text).unwrap();
    let timeline = back.timelines.values().next().unwrap();
    assert_eq!(timeline.tracks[0].mix.effects, vec![effect]);
}

#[test]
fn plugin_state_debug_is_compact() {
    let state = PluginState::new(vec![0u8; 1 << 20]);
    assert!(format!("{state:?}").len() < 64);
}

#[test]
fn fresh_ids_differ() {
    let ids: Vec<EffectId> = (0..1000).map(|_| EffectId::fresh()).collect();
    assert!(ids.iter().all(|id| id.0 != 0));
    let unique: std::collections::HashSet<EffectId> = ids.into_iter().collect();
    assert_eq!(unique.len(), 1000);
}

#[test]
fn content_hash_depends_on_the_states() {
    let a = vst3(vec![1, 2, 3]);
    let mut b = a.clone();
    assert_eq!(a.content_hash(), b.content_hash());
    b.controller_state = PluginState::new(vec![0]);
    assert_ne!(a.content_hash(), b.content_hash());
}

#[test]
fn with_fresh_ids_changes_only_vst3() {
    let native = AudioEffect::new(AudioEffectKind::Mono);
    assert_eq!(native.clone().with_fresh_ids(), native);

    let plugin = AudioEffect::new(AudioEffectKind::Vst3(vst3(vec![1])));
    let AudioEffectKind::Vst3(before) = plugin.kind.clone() else {
        unreachable!()
    };
    let AudioEffectKind::Vst3(after) = plugin.with_fresh_ids().kind else {
        unreachable!()
    };
    assert_ne!(before.id, after.id);
    assert_eq!(before.class_id, after.class_id);
    assert_eq!(before.vendor, after.vendor);
    assert_eq!(before.version, after.version);
    assert_eq!(before.component_state, after.component_state);
}

#[test]
fn timeline_uses_enabled_plugins_sees_an_enabled_vst3_on_any_strip() {
    let mut project = Project::default();
    let mut timeline = Timeline {
        name: "t".into(),
        fps: Rational::new(30, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Audio)],
        markers: Vec::new(),
        master: Default::default(),
    };
    let id = project.timelines.insert(timeline.clone());
    assert!(!project.timeline_uses_enabled_plugins(id));

    let mut bypassed = AudioEffect::new(AudioEffectKind::Vst3(vst3(vec![1])));
    bypassed.enabled = false;
    timeline.master.effects.push(bypassed);
    project.timelines[id] = timeline.clone();
    assert!(!project.timeline_uses_enabled_plugins(id));

    timeline.tracks[0]
        .mix
        .effects
        .push(AudioEffect::new(AudioEffectKind::Vst3(vst3(vec![1]))));
    project.timelines[id] = timeline;
    assert!(project.timeline_uses_enabled_plugins(id));
}

#[test]
fn plugin_state_and_effect_hashes_do_not_change() {
    let bytes: Vec<u8> = (0..1000).map(|i| (i * 31 % 251) as u8).collect();
    assert_eq!(
        PluginState::new(bytes.clone()).hash(),
        11_373_983_830_107_658_837
    );
    assert_eq!(PluginState::new(Vec::new()).hash(), 0xcbf2_9ce4_8422_2325);
    assert_eq!(vst3(bytes).content_hash(), 5_725_312_826_510_906_759);
}

#[test]
fn content_hash_ignores_id_name_and_vendor() {
    let original = vst3(vec![1, 2, 3]);
    let copy = Vst3Effect {
        id: EffectId::fresh(),
        name: "Renamed".into(),
        vendor: "Other".into(),
        ..original.clone()
    };
    assert_eq!(copy.content_hash(), original.content_hash());
    let other_version = Vst3Effect {
        version: "2.0".into(),
        ..original.clone()
    };
    assert_ne!(other_version.content_hash(), original.content_hash());
}

#[test]
fn plugin_state_new_takes_the_buffer_without_copying() {
    let bytes = vec![5u8; 1 << 16];
    let ptr = bytes.as_ptr();
    let state = PluginState::new(bytes);
    assert_eq!(state.bytes().as_ptr(), ptr);
}

#[test]
fn a_lowercase_class_id_from_a_saved_project_loads_canonical() {
    let mut effect = vst3(vec![1]);
    effect.class_id = effect.class_id.to_ascii_lowercase();
    let text = ron::ser::to_string(&effect).unwrap();
    let back: Vst3Effect = ron::from_str(&text).unwrap();
    assert_eq!(back.class_id, "0123456789ABCDEF0123456789ABCDEF");
}
