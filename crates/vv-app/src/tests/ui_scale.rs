use super::*;

fn draw(ctx: &egui::Context, events: Vec<egui::Event>, native_scale: f32) -> egui::FullOutput {
    let mut input = egui::RawInput {
        events,
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1000.0, 700.0) / ctx.zoom_factor(),
        )),
        ..Default::default()
    };
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .native_pixels_per_point = Some(native_scale);
    ctx.run_ui(input, show)
}

fn text_center(shapes: &[egui::epaint::ClippedShape], text: &str) -> egui::Pos2 {
    shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(shape) if shape.galley.job.text == text => {
                Some(shape.visual_bounding_rect().center())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing control: {text}"))
}

fn click(ctx: &egui::Context, position: egui::Pos2, native_scale: f32) {
    for pressed in [true, false] {
        draw(
            ctx,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            native_scale,
        )
        .textures_delta
        .clear();
    }
}

#[test]
fn scale_presets_and_reset_work_at_native_and_high_dpi() {
    for native_scale in [1.0, 2.0] {
        let ctx = egui::Context::default();
        configure(&ctx);
        for preset in PRESETS {
            let mut output = draw(&ctx, vec![], native_scale);
            let position = text_center(&output.shapes, &format!("{preset}%"));
            output.textures_delta.clear();
            click(&ctx, position, native_scale);
            let mut output = draw(&ctx, vec![], native_scale);
            assert!((ctx.zoom_factor() - preset as f32 / 100.0).abs() < 0.001);
            assert!((output.pixels_per_point - native_scale * preset as f32 / 100.0).abs() < 0.001);
            output.textures_delta.clear();
        }
        let mut output = draw(&ctx, vec![], native_scale);
        let position = text_center(&output.shapes, &t!("settings.ui_scale_reset"));
        output.textures_delta.clear();
        click(&ctx, position, native_scale);
        draw(&ctx, vec![], native_scale).textures_delta.clear();
        assert_eq!(ctx.zoom_factor(), 1.0);
    }
}

#[derive(Default)]
struct Storage(std::collections::HashMap<String, String>);

impl eframe::Storage for Storage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }

    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), value);
    }

    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }

    fn flush(&mut self) {}
}

#[test]
fn eframe_memory_keeps_custom_scale_without_another_preference() {
    let ctx = egui::Context::default();
    ctx.set_zoom_factor(1.37);
    draw(&ctx, vec![], 1.0).textures_delta.clear();
    let mut storage = Storage::default();
    ctx.memory(|memory| eframe::set_value(&mut storage, "egui", memory));
    let restored = egui::Context::default();
    restored.memory_mut(|memory| *memory = eframe::get_value(&storage, "egui").unwrap());
    configure(&restored);
    for _ in 0..3 {
        draw(&restored, vec![], 1.0).textures_delta.clear();
        assert_eq!(restored.zoom_factor(), 1.37);
    }
    assert!(!restored.options(|options| options.zoom_with_keyboard));
}

#[test]
fn old_egui_memory_keeps_the_default_scale() {
    let mut storage = Storage::default();
    eframe::Storage::set_string(&mut storage, "egui", "(options:())".to_owned());
    let ctx = egui::Context::default();
    ctx.memory_mut(|memory| *memory = eframe::get_value(&storage, "egui").unwrap());
    configure(&ctx);
    draw(&ctx, vec![], 1.0).textures_delta.clear();
    assert_eq!(ctx.zoom_factor(), 1.0);
}

#[test]
fn invalid_or_extreme_saved_scales_are_recoverable() {
    for (saved, expected) in [
        (0.0, 1.0),
        (-1.0, 1.0),
        (f32::NAN, 1.0),
        (f32::INFINITY, 1.0),
        (0.1, 0.2),
        (10.0, 5.0),
    ] {
        let ctx = egui::Context::default();
        ctx.options_mut(|options| options.zoom_factor = saved);
        configure(&ctx);
        draw(&ctx, vec![], 1.0).textures_delta.clear();
        assert_eq!(ctx.zoom_factor(), expected);
    }
}

#[test]
fn the_app_reset_shortcut_can_be_reassigned() {
    let ctx = egui::Context::default();
    configure(&ctx);
    ctx.set_zoom_factor(2.0);
    ctx.run_ui(egui::RawInput::default(), |_| {})
        .textures_delta
        .clear();
    let mut app = crate::VenturiApp::default();
    app.settings.keymap.assign(
        crate::settings::Action::ResetUiScale,
        Some(0),
        crate::settings::Shortcut::ctrl(egui::Key::Num9),
    );
    for (key, expected) in [(egui::Key::Num0, 2.0), (egui::Key::Num9, 1.0)] {
        let input = egui::RawInput {
            events: vec![
                egui::Event::ModifiersChanged(egui::Modifiers::COMMAND),
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                },
            ],
            ..Default::default()
        };
        ctx.run_ui(input, |ui| app.handle_shortcuts(ui))
            .textures_delta
            .clear();
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .textures_delta
            .clear();
        assert_eq!(ctx.zoom_factor(), expected);
    }
}

#[test]
fn appearance_controls_are_translated() {
    for locale in rust_i18n::available_locales!() {
        for key in [
            "settings.section_appearance",
            "settings.ui_scale",
            "settings.ui_scale_reset",
            "settings.ui_scale_hint",
        ] {
            assert!(
                crate::_rust_i18n_try_translate(&locale, key).is_some(),
                "{locale}: {key}"
            );
        }
    }
}
