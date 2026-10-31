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
fn clear_mark_shortcuts_use_alt_i_o_and_x() {
    let keymap = Keymap::default();
    for (action, key) in [
        (Action::ClearIn, egui::Key::I),
        (Action::ClearOut, egui::Key::O),
        (Action::ClearInOut, egui::Key::X),
    ] {
        assert_eq!(
            keymap.shortcuts(action),
            &[Shortcut {
                alt: true,
                ..Shortcut::plain(key)
            }]
        );
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
    settings.preview_resolution = PreviewResolution::Quarter;
    settings.lookahead_secs = 7.5;
    settings.behind_secs = 0.5;
    settings.cache_budget_bytes = 3_000_000_000;
    settings.audio_budget_bytes = 4_000_000_000;
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
    assert_eq!(loaded.preview_resolution, PreviewResolution::Full);
    assert_eq!(
        loaded.cache_budget_bytes,
        Settings::default().cache_budget_bytes
    );
    assert_eq!(
        loaded.audio_budget_bytes,
        Settings::default().audio_budget_bytes
    );
    assert_eq!(loaded.hw_decode, crate::hw_decode::HwDecodeMode::Auto);
    assert_eq!(
        loaded.hw_decode_budget_bytes, None,
        "follows the RAM until set"
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Ensures saved shortcuts keep priority over conflicting defaults during migration.
#[test]
fn loading_legacy_shortcuts_prioritizes_saved_bindings_over_defaults() {
    let path = std::env::temp_dir()
        .join(format!(
            "vv-settings-shortcut-migration-{}",
            std::process::id()
        ))
        .join("settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"shortcuts":{"cut":["Alt+V"]}}"#).unwrap();

    let loaded = Settings::load(&path);
    let alt_v = Shortcut {
        alt: true,
        ..Shortcut::plain(egui::Key::V)
    };

    assert_eq!(loaded.keymap.shortcuts(Action::Cut), &[alt_v]);
    assert!(loaded.keymap.shortcuts(Action::PasteAttributes).is_empty());
    for action in Action::ALL
        .into_iter()
        .filter(|action| *action != Action::Cut)
    {
        assert!(
            !loaded.keymap.shortcuts(action).contains(&alt_v),
            "{action:?} also uses {alt_v}"
        );
    }
    assert_eq!(
        loaded.keymap.shortcuts(Action::SelectFromPlayhead),
        &[Shortcut {
            alt: true,
            ..Shortcut::plain(egui::Key::Y)
        }]
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
        media_pool_layout: crate::media_pool::PoolLayout::Grid,
        library_layout: crate::media_pool::PoolLayout::List,
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

#[test]
fn an_audio_budget_out_of_range_in_the_file_is_clamped() {
    let path = std::env::temp_dir()
        .join(format!("vv-settings-audio-{}", std::process::id()))
        .join("settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    for (mb, bytes) in [(0, 100_000_000), (99_999, 32_000_000_000)] {
        std::fs::write(&path, format!(r#"{{"audio_budget_mb": {mb}}}"#)).unwrap();
        assert_eq!(Settings::load(&path).audio_budget_bytes, bytes);
    }
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn fast_playback_shown_frames_only_is_on_by_default_and_saved() {
    let path = std::env::temp_dir()
        .join(format!("vv-settings-stride-{}", std::process::id()))
        .join("settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{}").unwrap();
    assert!(Settings::load(&path).fast_playback_shown_frames_only);

    let mut settings = Settings::default();
    settings.fast_playback_shown_frames_only = false;
    settings.save(&path).unwrap();
    assert!(!Settings::load(&path).fast_playback_shown_frames_only);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn keyframes_when_behind_is_on_by_default_and_saved() {
    let path = std::env::temp_dir()
        .join(format!("vv-settings-keyframes-{}", std::process::id()))
        .join("settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{}").unwrap();
    assert!(Settings::load(&path).keyframes_when_behind);

    let mut settings = Settings::default();
    settings.keyframes_when_behind = false;
    settings.save(&path).unwrap();
    assert!(!Settings::load(&path).keyframes_when_behind);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn capture_waits_past_a_lone_modifier_for_the_whole_combination() {
    let key = |key, modifiers| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    };
    assert_eq!(
        Shortcut::captured(&key(egui::Key::ControlLeft, egui::Modifiers::COMMAND)),
        None
    );
    assert_eq!(
        Shortcut::captured(&key(egui::Key::K, egui::Modifiers::COMMAND)),
        Some(Shortcut::ctrl(egui::Key::K))
    );
}

fn load_json(tag: &str, json: &str) -> Settings {
    let path = std::env::temp_dir()
        .join(format!("vv-settings-{tag}-{}", std::process::id()))
        .join("settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, json).unwrap();
    let settings = Settings::load(&path);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    settings
}

#[test]
fn a_scan_that_found_nothing_is_not_scanned_again_at_startup() {
    let mut settings = Settings::default();
    assert!(settings.needs_plugin_scan());
    settings.vst3_plugins = Some(Vec::new());
    assert!(!settings.needs_plugin_scan());

    let path = std::env::temp_dir()
        .join(format!("vv-settings-scanned-{}", std::process::id()))
        .join("settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    settings.vst3_blocklist = vec![
        legacy("/x/Bad.vst3"),
        Blocked {
            path: "/x/New.vst3".into(),
            modified: Some(42),
        },
    ];
    settings.save(&path).unwrap();
    let loaded = Settings::load(&path);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert_eq!(loaded.vst3_plugins, Some(Vec::new()));
    assert!(!loaded.needs_plugin_scan());
    assert_eq!(loaded.vst3_blocklist, settings.vst3_blocklist);
}

#[test]
fn settings_from_before_the_flag_count_as_scanned_only_with_a_cached_list() {
    assert!(load_json("old-empty", "{}").needs_plugin_scan());
    let plugin = r#"{"class_id":"AA","name":"a","vendor":"v","version":"1","path":"/p/a.vst3"}"#;
    let old = load_json("old-list", &format!(r#"{{"vst3_plugins":[{plugin}]}}"#));
    assert!(!old.needs_plugin_scan());
}

#[test]
fn crashing_rescans_accumulate_the_blocklist() {
    let dir = std::env::temp_dir().join(format!("vv-settings-rescan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("scan.marker");
    let (x, y) = (PathBuf::from("/x/X.vst3"), PathBuf::from("/x/Y.vst3"));
    let mut settings = Settings::default();

    std::fs::write(&marker, b"/x/X.vst3").unwrap();
    settings.prepare_plugin_scan(true, &marker);
    assert_eq!(settings.blocked_paths(), vec![x.clone()]);
    assert!(!marker.exists());

    std::fs::write(&marker, b"/x/Y.vst3").unwrap();
    settings.prepare_plugin_scan(true, &marker);
    settings.prepare_plugin_scan(true, &marker);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(settings.blocked_paths(), vec![x, y]);
}

fn legacy(path: impl Into<PathBuf>) -> Blocked {
    Blocked {
        path: path.into(),
        modified: None,
    }
}

#[test]
fn a_blocked_bundle_is_retried_only_once_it_changed_on_disk() {
    let dir = std::env::temp_dir().join(format!("vv-settings-retry-{}", std::process::id()));
    let bundle = dir.join("Crash.vst3");
    #[cfg(target_os = "macos")]
    let binary = bundle.join("Contents/MacOS/Crash");
    #[cfg(not(target_os = "macos"))]
    let binary = bundle.join(format!(
        "Contents/{}-linux/Crash.so",
        std::env::consts::ARCH
    ));
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"").unwrap();
    let marker = dir.join("scan.marker");
    let mut settings = Settings::default();
    settings.vst3_blocklist = vec![Blocked::new(bundle.clone()), legacy("/x/Old.vst3")];
    let recorded = settings.vst3_blocklist[0].modified;
    assert!(recorded.is_some());

    settings.prepare_plugin_scan(true, &marker);
    assert_eq!(settings.vst3_blocklist.len(), 2);

    let later = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_secs(recorded.unwrap() + 1000);
    std::fs::File::open(&binary)
        .unwrap()
        .set_modified(later)
        .unwrap();
    settings.prepare_plugin_scan(false, &marker);
    assert_eq!(settings.vst3_blocklist.len(), 2);
    settings.prepare_plugin_scan(true, &marker);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(settings.vst3_blocklist, vec![legacy("/x/Old.vst3")]);
}

#[test]
fn a_path_only_blocklist_entry_loads_as_legacy() {
    let loaded = load_json(
        "legacy-blocklist",
        r#"{"vst3_blocklist":["/x/A.vst3",{"path":"/x/B.vst3","modified":7}]}"#,
    );
    assert_eq!(
        loaded.vst3_blocklist,
        vec![
            legacy("/x/A.vst3"),
            Blocked {
                path: "/x/B.vst3".into(),
                modified: Some(7)
            }
        ]
    );
}

fn plugin(name: &str, path: PathBuf) -> vv_plugin::PluginInfo {
    vv_plugin::PluginInfo {
        class_id: name.repeat(2).to_uppercase(),
        name: name.into(),
        vendor: "v".into(),
        version: "1".into(),
        path,
    }
}

#[cfg(unix)]
#[test]
fn a_non_utf8_plugin_path_does_not_fail_the_save() {
    use std::os::unix::ffi::OsStringExt;
    let bad = PathBuf::from(std::ffi::OsString::from_vec(b"/p/b\xff.vst3".to_vec()));
    let mut settings = Settings::default();
    settings.kinetic_scroll = false;
    settings.vst3_plugins = Some(vec![
        plugin("a", PathBuf::from("/p/a.vst3")),
        plugin("b", bad.clone()),
    ]);
    settings.vst3_blocklist = vec![legacy(bad), legacy("/x/Bad.vst3")];

    let path = std::env::temp_dir()
        .join(format!("vv-settings-nonutf8-{}", std::process::id()))
        .join("settings.json");
    settings.save(&path).unwrap();
    let loaded = Settings::load(&path);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert!(!loaded.kinetic_scroll);
    assert_eq!(
        loaded.vst3_plugins,
        Some(vec![plugin("a", PathBuf::from("/p/a.vst3"))])
    );
    assert_eq!(loaded.vst3_blocklist, vec![legacy("/x/Bad.vst3")]);
}

#[test]
fn one_malformed_plugin_entry_does_not_reset_the_settings() {
    let good = r#"{"class_id":"AA","name":"a","vendor":"v","version":"1","path":"/p/a.vst3"}"#;
    let json = format!(
        r#"{{"kinetic_scroll":false,"vst3_plugins":[{{"name":1}},{good}],"vst3_blocklist":[5,"/x/Bad.vst3"]}}"#
    );
    let loaded = load_json("bad-entry", &json);
    assert!(!loaded.kinetic_scroll);
    assert_eq!(
        loaded.vst3_plugins,
        Some(vec![plugin("a", PathBuf::from("/p/a.vst3"))])
    );
    assert_eq!(loaded.vst3_blocklist, vec![legacy("/x/Bad.vst3")]);
    assert!(!loaded.needs_plugin_scan());
}

#[test]
fn the_scanned_state_loads_as_before_from_every_file_shape() {
    let a = r#"{"class_id":"AA","name":"a","vendor":"v","version":"1","path":"/p/a.vst3"}"#;
    let list = Some(vec![plugin("a", PathBuf::from("/p/a.vst3"))]);
    let cases = [
        ("{}".to_owned(), None),
        (r#"{"vst3_plugins":[]}"#.to_owned(), None),
        (format!(r#"{{"vst3_plugins":[{a}]}}"#), list.clone()),
        (
            r#"{"vst3_plugins":[],"vst3_scanned":true}"#.to_owned(),
            Some(Vec::new()),
        ),
        (
            r#"{"vst3_plugins":[],"vst3_scanned":false}"#.to_owned(),
            None,
        ),
        (
            format!(r#"{{"vst3_plugins":[{a}],"vst3_scanned":true}}"#),
            list.clone(),
        ),
    ];
    for (i, (json, expected)) in cases.into_iter().enumerate() {
        let loaded = load_json(&format!("scanned-{i}"), &json);
        assert_eq!(loaded.vst3_plugins, expected, "{json}");
        assert_eq!(loaded.needs_plugin_scan(), expected.is_none(), "{json}");
    }
}

#[test]
fn the_scanned_state_is_written_as_before() {
    let path = std::env::temp_dir()
        .join(format!("vv-settings-written-{}", std::process::id()))
        .join("settings.json");
    let written = |plugins| {
        let settings = Settings {
            vst3_plugins: plugins,
            ..Default::default()
        };
        settings.save(&path).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        (json["vst3_plugins"].clone(), json["vst3_scanned"].clone())
    };
    assert_eq!(
        written(None),
        (serde_json::json!([]), serde_json::json!(false))
    );
    assert_eq!(
        written(Some(Vec::new())),
        (serde_json::json!([]), serde_json::json!(true))
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
