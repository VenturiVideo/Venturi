use super::*;

#[test]
fn mark_items_need_markable_content() {
    let mut app = VenturiApp::default();
    assert_eq!(app.mark_target_frames(), 0);
    let timeline = app.ensure_timeline();
    assert_eq!(app.mark_target_frames(), 0);
    let id = app.session.project.alloc_clip_id();
    app.session.project.timelines[timeline].tracks[0]
        .clips
        .push(vv_core::Clip::from_source_range(
            id,
            vv_core::ClipSource::SolidColor,
            0,
            100,
            0,
            vv_core::Rational::one(),
        ));
    assert_eq!(app.mark_target_frames(), 100);
    // An active preview without media must not fall back to the timeline.
    app.browsing = Some(Browsed::Asset(String::new()));
    assert_eq!(app.mark_target_frames(), 0);
}

/// Checks that clicking the File menu's Exit item requests a normal close.
#[test]
fn exit_menu_item_requests_window_close() {
    let app = VenturiApp::default();
    let ctx = egui::Context::default();
    let mut first_frame = egui::RawInput::default();
    first_frame.screen_rect = Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(800.0, 600.0),
    ));
    let mut exit_rect = None;
    let mut output = ctx.run_ui(first_frame, |ui| {
        exit_rect = Some(app.show_exit_menu_item(ui).rect);
    });
    output.textures_delta.clear();
    let exit_rect = exit_rect.expect("Exit item was drawn");
    let position = exit_rect.center();

    let mut press = egui::RawInput::default();
    press.screen_rect = Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(800.0, 600.0),
    ));
    press.events.extend([
        egui::Event::PointerMoved(position),
        egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    let mut output = ctx.run_ui(press, |ui| {
        app.show_exit_menu_item(ui);
    });
    output.textures_delta.clear();

    let mut release = egui::RawInput::default();
    release.screen_rect = Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(800.0, 600.0),
    ));
    release.events.extend([
        egui::Event::PointerMoved(position),
        egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    let mut output = ctx.run_ui(release, |ui| {
        app.show_exit_menu_item(ui);
    });

    let requested_close = output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .is_some_and(|viewport| viewport.commands.contains(&egui::ViewportCommand::Close));
    output.textures_delta.clear();
    assert!(requested_close);
}
