use super::*;

#[test]
fn shortcuts_round_trip_through_the_config_format() {
    for action in Action::ALL {
        for shortcut in action.default_shortcuts() {
            assert_eq!(Shortcut::from_config(&shortcut.to_config()), Some(shortcut));
        }
    }
    assert_eq!(
        Shortcut::from_config("Ctrl++"),
        Some(Shortcut::ctrl(egui::Key::Plus))
    );
}

#[test]
fn every_action_label_is_translated() {
    for locale in rust_i18n::available_locales!() {
        for action in Action::ALL {
            let key = format!("action.{}", action.id());
            assert!(
                crate::_rust_i18n_try_translate(&locale, &key).is_some(),
                "{locale}: {key}"
            );
        }
    }
}

#[test]
fn default_shortcuts_are_unique() {
    let keymap = Keymap::default();
    let mut seen = Vec::new();
    for action in Action::ALL {
        for shortcut in keymap.shortcuts(action) {
            assert!(!seen.contains(shortcut), "{shortcut} used twice");
            seen.push(*shortcut);
        }
    }
}

#[test]
fn assigning_a_shortcut_takes_it_away_from_other_actions() {
    let mut keymap = Keymap::default();
    let stolen = keymap.assign(Action::Split, Some(0), Shortcut::plain(egui::Key::D));
    assert_eq!(stolen, vec![Action::ToggleDisabled]);
    assert_eq!(
        keymap.shortcuts(Action::Split),
        &[Shortcut::plain(egui::Key::D)]
    );
    assert!(keymap.shortcuts(Action::ToggleDisabled).is_empty());
}

#[test]
fn save_then_load_keeps_custom_shortcuts_and_defaults_for_the_rest() {
    let path = std::env::temp_dir()
        .join(format!("vv-settings-{}", std::process::id()))
        .join("settings.json");
    let mut settings = Settings::default();
    settings
        .keymap
        .assign(Action::Split, Some(0), Shortcut::ctrl(egui::Key::K));
    settings.keymap.remove(Action::ZoomIn, 1);
    settings.language = Language::Italian;
    settings.proxy_enabled = true;
    settings.proxy_quality = ProxyQuality::High;
    settings.lookahead_secs = 7.5;
    settings.behind_secs = 0.5;
    settings.cache_budget_bytes = 3_000_000_000;
    settings.hw_decode = crate::hw_decode::HwDecodeMode::Vulkan;
    settings.hw_decode_budget_bytes = Some(2_000_000_000);
    settings.save(&path).unwrap();

    let loaded = Settings::load(&path);
    assert_eq!(loaded, settings);

    std::fs::write(
        &path,
        r#"{"shortcuts": {"split": ["Alt+K"]}, "proxy_enabled": true}"#,
    )
    .unwrap();
    let loaded = Settings::load(&path);
    assert_eq!(
        loaded.keymap.shortcuts(Action::Split),
        &[Shortcut {
            alt: true,
            ..Shortcut::plain(egui::Key::K)
        }]
    );
    assert_eq!(
        loaded.keymap.shortcuts(Action::Undo),
        Keymap::default().shortcuts(Action::Undo)
    );
    assert!(!loaded.proxy_enabled);
    assert_eq!(loaded.proxy_quality, ProxyQuality::default());
    assert_eq!(
        loaded.cache_budget_bytes,
        Settings::default().cache_budget_bytes
    );
    assert_eq!(loaded.hw_decode, crate::hw_decode::HwDecodeMode::Auto);
    assert_eq!(
        loaded.hw_decode_budget_bytes, None,
        "follows the RAM until set"
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn save_then_load_keeps_custom_panel_layout_and_defaults_it_when_absent() {
    let path = std::env::temp_dir()
        .join(format!("vv-settings-panels-{}", std::process::id()))
        .join("settings.json");
    let mut settings = Settings::default();
    settings.panels = PanelLayout {
        media_pool_open: false,
        effects_open: true,
        inspector_open: false,
        keyframe_editor_open: true,
        mixer_open: true,
        color_window_open: true,
        color_scopes: [
            vv_render::ScopeKind::Histogram,
            vv_render::ScopeKind::Parade,
        ],
        left_column_width: 321.0,
        media_pool_fraction: 0.3,
        inspector_width: 456.0,
        timeline_height: 199.0,
    };
    settings.save(&path).unwrap();

    let loaded = Settings::load(&path);
    assert_eq!(loaded, settings);

    std::fs::write(&path, "{}").unwrap();
    let loaded = Settings::load(&path);
    assert_eq!(loaded.panels, PanelLayout::default());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn shift_is_ignored_on_symbols_but_not_on_letters() {
    let ctx = egui::Context::default();
    let press = |key, modifiers| {
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::ModifiersChanged(modifiers));
        input.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        });
        let mut result = (false, false);
        let mut output = ctx.run_ui(input, |ui| {
            ui.input(|i| {
                result = (
                    Shortcut::ctrl(egui::Key::Plus).pressed(i),
                    Shortcut::ctrl(egui::Key::S).pressed(i),
                )
            });
        });
        output.textures_delta.clear();
        result
    };
    let ctrl_shift = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
    assert_eq!(press(egui::Key::Plus, ctrl_shift), (true, false));
    assert_eq!(press(egui::Key::S, ctrl_shift), (false, false));
    assert_eq!(press(egui::Key::S, egui::Modifiers::COMMAND), (false, true));
}

#[test]
fn effect_presets_are_saved_and_a_broken_one_is_dropped_alone() {
    let path = std::env::temp_dir()
        .join(format!("vv-settings-presets-{}", std::process::id()))
        .join("settings.json");
    let mut settings = Settings::default();
    let mut params = vv_core::MultibandCompressor::DEFAULT;
    params.bands[1].ratio = 5.0;
    settings.compressor_presets = vec![Preset {
        name: "Mine".into(),
        params,
    }];
    let mut eq = vv_core::Equalizer::DEFAULT;
    eq.bands[2].gain_db = -4.0;
    settings.eq_presets = vec![Preset {
        name: "Mine".into(),
        params: eq,
    }];
    settings.kinetic_scroll = false;
    settings.input_device = Some("USB Microphone".into());
    settings.recording_format = Some(vv_media::audio_file::AudioFileFormat::Flac);
    settings.recording_dir = Some("/tmp/takes".into());
    settings.save(&path).unwrap();
    assert_eq!(Settings::load(&path), settings);

    let text = std::fs::read_to_string(&path).unwrap();
    let mut json: serde_json::Value = serde_json::from_str(&text).unwrap();
    json["compressor_presets"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({ "name": "Old", "params": { "bands": 3 } }));
    json["eq_presets"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({ "name": "Old", "params": { "bands": [] } }));
    std::fs::write(&path, json.to_string()).unwrap();
    let loaded = Settings::load(&path);
    assert_eq!(loaded.compressor_presets, settings.compressor_presets);
    assert_eq!(loaded.eq_presets, settings.eq_presets);
    assert!(!loaded.kinetic_scroll, "the rest of the file still reads");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
