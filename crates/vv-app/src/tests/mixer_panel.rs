use super::*;

#[test]
fn fader_curve_puts_unity_at_its_mark_and_spans_the_whole_gain_range() {
    assert_eq!(fader_position(0.0), 0.7);
    assert_eq!(fader_position(vv_core::GAIN_DB_MIN), 0.0);
    assert_eq!(fader_position(vv_core::GAIN_DB_MAX), 1.0);
    assert_eq!(fader_position(-1000.0), 0.0, "clamped");
}

#[test]
fn fader_db_inverts_fader_position() {
    for db in [-100.0, -73.5, -30.0, -7.2, 0.0, 4.5, 30.0] {
        assert!((fader_db(fader_position(db)) - db).abs() < 1e-3, "{db}");
    }
}

#[test]
fn pan_reads_as_centre_or_side_percent() {
    assert_eq!(format_pan(0.0), "C");
    assert_eq!(format_pan(0.001), "C");
    assert_eq!(format_pan(-0.3), "L30");
    assert_eq!(format_pan(1.0), "R100");
}

#[test]
fn the_settings_window_follows_the_removal_of_an_effect_above_it() {
    let timeline = <vv_core::TimelineId as vv_core::Id>::from_raw(1);
    let channel = MixerChannel::Track(2);
    let mut state = MixerPanelState::default();
    let remove = |index| StripEdit {
        remove_effect: Some(index),
        ..StripEdit::default()
    };

    state.follow_edit(
        &StripEdit {
            open_settings: Some(2),
            ..StripEdit::default()
        },
        timeline,
        channel,
    );
    state.follow_edit(&remove(3), timeline, channel);
    state.follow_edit(&remove(0), timeline, MixerChannel::Master);
    assert_eq!(state.open_effect, Some((timeline, channel, 2)));
    state.follow_edit(&remove(0), timeline, channel);
    assert_eq!(state.open_effect, Some((timeline, channel, 1)));
    state.follow_edit(&remove(1), timeline, channel);
    assert_eq!(state.open_effect, None);
}

#[test]
fn dropping_in_the_gaps_around_an_effect_leaves_it_where_it_is() {
    assert_eq!(moved_to(1, 1), None);
    assert_eq!(moved_to(1, 2), None);
    assert_eq!(moved_to(1, 0), Some(0));
    assert_eq!(moved_to(0, 3), Some(2), "to the end of three");
    assert_eq!(moved_to(2, 1), Some(1));
}

#[test]
fn the_other_effects_shift_around_a_moved_one() {
    // [a b c d], b to the end: [a c d b].
    let after: Vec<usize> = (0..4).map(|i| index_after_move(i, 1, 3)).collect();
    assert_eq!(after, [0, 3, 1, 2]);
    // [a b c d], d to the front: [d a b c].
    let after: Vec<usize> = (0..4).map(|i| index_after_move(i, 3, 0)).collect();
    assert_eq!(after, [1, 2, 3, 0]);
}

fn info(class: &str, vendor: &str, name: &str) -> vv_plugin::PluginInfo {
    vv_plugin::PluginInfo {
        class_id: class.into(),
        name: name.into(),
        vendor: vendor.into(),
        ..vv_plugin::testing::fixture_info()
    }
}

#[test]
fn vst3_menu_groups_by_vendor() {
    let list = [
        info("3", "Zed", "Alpha"),
        info("2", "Acme", "Zeta"),
        info("1", "Acme", "Beta"),
    ];
    let menu = vst3_menu(&list);
    let names: Vec<(&str, Vec<&str>)> = menu
        .iter()
        .map(|(v, p)| (v.as_str(), p.iter().map(|i| i.name.as_str()).collect()))
        .collect();
    assert_eq!(
        names,
        [("Acme", vec!["Beta", "Zeta"]), ("Zed", vec!["Alpha"])]
    );
}

#[test]
fn add_plugin_effect_has_fresh_id() {
    let plugin = info("ab", "Acme", "Comp");
    let (a, b) = (add_plugin_effect(&plugin), add_plugin_effect(&plugin));
    let (AudioEffectKind::Vst3(a), AudioEffectKind::Vst3(b)) = (a, b) else {
        panic!("not a plugin");
    };
    assert_ne!(a.id, b.id);
    assert_eq!((a.class_id.as_str(), a.name.as_str()), ("AB", "Comp"));
    assert_eq!(a.component_state, vv_core::PluginState::default());
}

#[test]
fn a_vst3_row_is_badged_when_missing_or_failed_to_load() {
    assert_eq!(vst3_badge(false, false), None);
    assert_eq!(vst3_badge(true, false), Some(Vst3Badge::Missing));
    assert_eq!(vst3_badge(false, true), Some(Vst3Badge::Failed));
    assert_eq!(vst3_badge(true, true), Some(Vst3Badge::Missing));
}

fn plugin(id: u64) -> AudioEffect {
    AudioEffect::new(AudioEffectKind::Vst3(Vst3Effect {
        id: vv_core::EffectId(id),
        name: "Fake".into(),
        ..vv_plugin::testing::fixture_effect_of(&"00".repeat(16), &[])
    }))
}

/// Draws the effects of every strip of `timeline` and returns the keys the
/// host was asked about.
fn keys_asked(timeline: &Timeline, known: bool) -> Vec<u64> {
    let asked = std::cell::RefCell::new(Vec::new());
    let latency = |key| {
        asked.borrow_mut().push(key);
        None
    };
    let failed = |_| false;
    let known = |_: &Vst3Effect| known;
    let view = Vst3View {
        installed: &[],
        scanning: false,
        known: &known,
        latency_ms: &latency,
        failed: &failed,
    };
    let keys = vv_audio::plugin_keys_in(0, timeline);
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let channels = (0..timeline.tracks.len())
            .map(MixerChannel::Track)
            .chain([MixerChannel::Master]);
        for channel in channels {
            let strip = channel_strip(timeline, channel).unwrap();
            effects(
                ui,
                &keys,
                channel,
                &strip.effects,
                0,
                &view,
                &mut StripEdit::default(),
            );
        }
    });
    output.textures_delta.clear();
    asked.into_inner()
}

fn plugin_timeline() -> Timeline {
    let strip = |effects| {
        let mut track = vv_core::Track::new(TrackKind::Audio);
        track.mix.effects = effects;
        track
    };
    let mut timeline = Timeline {
        name: "T".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (64, 36),
        tracks: vec![strip(vec![plugin(7), plugin(7)]), strip(vec![plugin(7)])],
        markers: Vec::new(),
        master: Default::default(),
    };
    timeline.master.effects = vec![plugin(9), plugin(7)];
    timeline
}

#[test]
fn the_rows_ask_the_host_with_the_keys_of_plugin_key_in() {
    let timeline = plugin_timeline();
    let expected: Vec<u64> = [
        (MixerChannel::Track(0), 0),
        (MixerChannel::Track(0), 1),
        (MixerChannel::Track(1), 0),
        (MixerChannel::Master, 0),
        (MixerChannel::Master, 1),
    ]
    .into_iter()
    .map(|(channel, index)| plugin_key_of(&timeline, channel, index))
    .collect();
    assert_eq!(keys_asked(&timeline, true), expected);
    assert_eq!(
        expected
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        5
    );
}

#[test]
fn the_not_installed_badge_follows_the_pools_known() {
    let timeline = plugin_timeline();
    assert!(keys_asked(&timeline, false).is_empty());
}
