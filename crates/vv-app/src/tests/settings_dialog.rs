use super::*;

fn appearance_dialog() -> SettingsDialog {
    SettingsDialog {
        mcp_forced: false,
        section: Section::Appearance,
        capture: None,
        notice: None,
        input_devices: Vec::new(),
        recording_formats: Vec::new(),
    }
}

#[test]
fn appearance_uses_egui_scale_without_changing_other_preferences() {
    let ctx = egui::Context::default();
    crate::ui_scale::configure(&ctx);
    ctx.set_zoom_factor(1.37);
    let mut settings = Settings::default();
    settings.language = Language::Italian;
    settings.lookahead_secs = 7.5;
    let before = settings.clone();
    let mut dialog = appearance_dialog();
    for _ in 0..3 {
        ctx.run_ui(egui::RawInput::default(), |_| {
            let response = dialog.show(&ctx, &mut settings);
            assert!(response.open);
            assert!(!response.changed);
        })
        .textures_delta
        .clear();
    }
    assert_eq!(settings, before);
    assert_eq!(ctx.zoom_factor(), 1.37);
}

#[test]
fn appearance_help_can_be_scrolled_into_a_small_window_at_double_scale() {
    let ctx = egui::Context::default();
    crate::ui_scale::configure(&ctx);
    ctx.set_zoom_factor(2.0);
    let mut settings = Settings::default();
    let mut dialog = appearance_dialog();
    let mut hint = None;
    for frame in 0..12 {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 480.0) / ctx.zoom_factor(),
            )),
            time: Some(frame as f64 / 30.0),
            ..Default::default()
        };
        if frame >= 3 {
            input
                .events
                .push(egui::Event::PointerMoved(egui::pos2(260.0, 160.0)));
            input.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -120.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let mut output = ctx.run_ui(input, |_| {
            dialog.show(&ctx, &mut settings);
        });
        hint = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == t!("settings.ui_scale_hint") => {
                Some((text.visual_bounding_rect(), shape.clip_rect))
            }
            _ => None,
        });
        output.textures_delta.clear();
    }
    let (hint, clip) = hint.expect("appearance help was drawn");
    assert!(
        hint.bottom() <= clip.bottom() + 1.0,
        "help {hint:?} is clipped by {clip:?}"
    );
    assert!(clip.intersects(ctx.content_rect()));
}
