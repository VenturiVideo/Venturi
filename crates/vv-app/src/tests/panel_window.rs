use super::*;

#[derive(Debug)]
struct DroppedClip;

impl egui::DroppedFile for DroppedClip {
    fn path(&self) -> &std::path::Path {
        std::path::Path::new("native-panel-clip.mp4")
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("Only the file handle is forwarded".into())
    }
}

#[test]
fn properties_remain_visible_on_embedded_backends() {
    let ctx = Context::default();
    let mut placement = Placement {
        detached: true,
        size: [640.0, 480.0],
    };
    let mut open = true;
    let mut drawn = false;
    let mut output = ctx.run_ui(RawInput::default(), |ui| {
        assert!(
            show_right(ui, "Properties", 300.0, &mut open, &mut placement, |ui| {
                ui.label("Shared content");
                drawn = true;
            })
            .is_some()
        );
    });
    output.textures_delta.clear();
    assert!(drawn && open);
    assert!(!placement.detached);
}

#[test]
fn child_file_drops_reach_the_root_once_even_without_keyboard_focus() {
    let ctx = Context::default();
    let file: egui::DroppedFileHandle = std::sync::Arc::new(DroppedClip);
    let mut output = ctx.run_ui(
        RawInput {
            focused: false,
            dropped_files: vec![file.clone()],
            ..Default::default()
        },
        |ui| {
            record_input(ui, "properties");
            record_input(ui, "properties");
        },
    );
    output.textures_delta.clear();
    let mut child = RawInput {
        viewport_id: ViewportId::from_hash_of("child"),
        ..Default::default()
    };
    forward_to_root(&ctx, &mut child);
    assert!(child.dropped_files.is_empty());
    let mut root = RawInput::default();
    forward_to_root(&ctx, &mut root);
    assert_eq!(root.dropped_files.len(), 1);
    assert!(std::sync::Arc::ptr_eq(&root.dropped_files[0], &file));
    let mut next = RawInput::default();
    forward_to_root(&ctx, &mut next);
    assert!(next.dropped_files.is_empty());
}

#[test]
fn embedded_backends_keep_a_detached_panel_visible() {
    let ctx = Context::default();
    let mut placement = Placement {
        detached: true,
        size: [640.0, 480.0],
    };
    let mut open = true;
    let mut drawn = false;
    let mut input = RawInput::default();
    input.screen_rect = Some(Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(1000.0, 800.0),
    ));
    let mut output = ctx.run_ui(input, |ui| {
        let rect = show(
            ui.ctx(),
            "audio_mixer",
            "Mixer",
            [420.0, 480.0],
            &mut open,
            &mut placement,
            |ui| {
                ui.label("Shared panel content");
                drawn = true;
            },
        );
        assert!(rect.is_some());
    });
    output.textures_delta.clear();
    assert!(drawn && open);
    assert!(!placement.detached);
}

#[test]
fn invalid_restored_sizes_fall_back_to_visible_defaults() {
    for size in [
        [f32::NAN, 640.0],
        [-10.0, 500.0],
        [100000.0, 500.0],
        [0.0, 0.0],
    ] {
        assert_eq!(
            Placement {
                detached: true,
                size
            }
            .size([720.0, 340.0]),
            [720.0, 340.0]
        );
    }
    assert_eq!(
        Placement {
            detached: true,
            size: [900.0, 600.0]
        }
        .size([720.0, 340.0]),
        [900.0, 600.0]
    );
}

#[test]
fn detached_keys_reach_the_root_and_release_when_the_panel_loses_focus() {
    let ctx = Context::default();
    let modifiers = Modifiers {
        ctrl: true,
        ..Default::default()
    };
    let press = Event::Key {
        key: Key::Z,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    };
    ctx.data_mut(|d| {
        d.insert_temp(
            Id::new("panel-forwarded"),
            Forwarded {
                events: vec![press],
                keys: vec![Key::Z],
                modifiers,
                source: Some("audio_mixer"),
            },
        )
    });
    let mut input = RawInput {
        focused: false,
        ..Default::default()
    };
    assert_eq!(
        forward_to_root(&ctx, &mut input),
        (true, Some("audio_mixer"))
    );
    assert!(input.focused);
    assert!(
        input
            .events
            .iter()
            .any(|e| matches!(e, Event::ModifiersChanged(m) if m.ctrl))
    );
    assert_eq!(
        input
            .events
            .iter()
            .filter(|e| matches!(
                e,
                Event::Key {
                    key: Key::Z,
                    pressed: true,
                    ..
                }
            ))
            .count(),
        1
    );
    let mut next = RawInput {
        focused: false,
        ..Default::default()
    };
    assert_eq!(forward_to_root(&ctx, &mut next), (true, None));
    assert!(next.events.iter().any(|e| matches!(
        e,
        Event::Key {
            key: Key::Z,
            pressed: false,
            ..
        }
    )));
}

#[test]
fn a_child_hook_does_not_consume_another_panels_forwarded_input() {
    let ctx = Context::default();
    ctx.data_mut(|d| {
        d.insert_temp(
            Id::new("panel-forwarded"),
            Forwarded {
                source: Some("color_window"),
                ..Default::default()
            },
        )
    });
    let mut child = RawInput {
        viewport_id: ViewportId::from_hash_of("child"),
        ..Default::default()
    };
    assert_eq!(forward_to_root(&ctx, &mut child), (false, None));
    let mut root = RawInput::default();
    assert_eq!(
        forward_to_root(&ctx, &mut root),
        (true, Some("color_window"))
    );
    assert_eq!(forward_to_root(&ctx, &mut root), (true, None));
    assert_eq!(forward_to_root(&ctx, &mut root), (false, None));
}

#[test]
fn editing_panel_text_does_not_forward_delete_to_the_timeline() {
    let ctx = Context::default();
    let mut text = String::from("12");
    let field = Id::new("panel-text");
    let mut output = ctx.run_ui(RawInput::default(), |ui| {
        ui.add(egui::TextEdit::singleline(&mut text).id(field))
            .request_focus();
    });
    output.textures_delta.clear();
    let mut input = RawInput::default();
    input.events.push(Event::Key {
        key: Key::Delete,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    });
    let mut output = ctx.run_ui(input, |ui| {
        ui.add(egui::TextEdit::singleline(&mut text).id(field));
        record_input(ui, "audio_mixer");
    });
    output.textures_delta.clear();
    let forwarded = ctx
        .data_mut(|d| d.remove_temp::<Forwarded>(Id::new("panel-forwarded")))
        .unwrap();
    assert_eq!(forwarded.source, Some("audio_mixer"));
    assert!(forwarded.events.is_empty() && forwarded.keys.is_empty());
}
