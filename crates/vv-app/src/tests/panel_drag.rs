use super::*;

fn input(id: ViewportId, scale: f32, events: Vec<Event>) -> RawInput {
    let source = ViewportId::from_hash_of("source");
    let mut raw = RawInput {
        viewport_id: id,
        events,
        ..Default::default()
    };
    raw.viewports.insert(
        source,
        egui::ViewportInfo {
            parent: Some(ViewportId::ROOT),
            inner_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(300.0, 300.0),
            )),
            native_pixels_per_point: Some(1.0),
            ..Default::default()
        },
    );
    raw.viewports.insert(
        ViewportId::ROOT,
        egui::ViewportInfo {
            inner_rect: Some(egui::Rect::from_min_size(
                egui::pos2(500.0, 0.0),
                egui::vec2(300.0, 300.0),
            )),
            native_pixels_per_point: Some(scale),
            ..Default::default()
        },
    );
    raw.screen_rect = Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(300.0, 300.0),
    ));
    raw
}

fn press() -> Event {
    Event::PointerButton {
        pos: egui::pos2(10.0, 10.0),
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    }
}

fn start(ctx: &Context, scale: f32) {
    let source = ViewportId::from_hash_of("source");
    let mut output = ctx.run_ui(
        input(
            source,
            scale,
            vec![Event::PointerMoved(egui::pos2(10.0, 10.0)), press()],
        ),
        |ui| {
            egui::DragAndDrop::set_payload(ui.ctx(), Generator::SolidColor);
        },
    );
    output.textures_delta.clear();
}

#[test]
fn native_drag_releases_once_in_the_destination_at_its_pixel_scale() {
    for scale in [1.0, 2.0] {
        let ctx = Context::default();
        ctx.add_plugin(NativeDrag::default());
        let mut output = ctx.run_ui(
            input(
                ViewportId::ROOT,
                scale,
                vec![Event::PointerMoved(egui::pos2(50.0, 50.0))],
            ),
            |ui| {
                ui.interact(ui.max_rect(), ui.id().with("drop"), egui::Sense::hover());
            },
        );
        output.textures_delta.clear();
        start(&ctx, scale);
        let point = egui::pos2(550.0 * scale, 50.0 * scale);
        let mut output = ctx.run_ui(
            input(
                ViewportId::from_hash_of("source"),
                scale,
                vec![
                    Event::PointerMoved(point),
                    Event::PointerButton {
                        pos: point,
                        button: PointerButton::Primary,
                        pressed: false,
                        modifiers: Default::default(),
                    },
                ],
            ),
            |_| {},
        );
        output.textures_delta.clear();
        assert!(
            !egui::DragAndDrop::has_any_payload(&ctx),
            "egui cleared the source release"
        );
        let mut output = ctx.run_ui(input(ViewportId::ROOT, scale, vec![]), |ui| {
            assert_eq!(
                ui.input(|i| i.pointer.interact_pos()),
                Some(egui::pos2(50.0, 50.0))
            );
            let response = ui.interact(ui.max_rect(), ui.id().with("drop"), egui::Sense::hover());
            let dropped = response.dnd_release_payload::<Generator>();
            assert_eq!(dropped.as_deref(), Some(&Generator::SolidColor));
        });
        output.textures_delta.clear();
        let mut output = ctx.run_ui(input(ViewportId::ROOT, scale, vec![]), |ui| {
            assert!(!ui.input(|i| i.pointer.any_released()));
            assert!(!egui::DragAndDrop::has_any_payload(ui.ctx()));
        });
        output.textures_delta.clear();
    }
}

#[test]
fn native_drag_escape_and_drop_outside_the_application_cancel_delivery() {
    for escape in [true, false] {
        let ctx = Context::default();
        ctx.add_plugin(NativeDrag::default());
        start(&ctx, 1.0);
        let event = if escape {
            Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }
        } else {
            Event::PointerButton {
                pos: egui::pos2(2000.0, 100.0),
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }
        };
        let mut output = ctx.run_ui(
            input(ViewportId::from_hash_of("source"), 1.0, vec![event]),
            |_| {},
        );
        output.textures_delta.clear();
        let mut output = ctx.run_ui(input(ViewportId::ROOT, 1.0, vec![]), |ui| {
            assert!(!egui::DragAndDrop::has_any_payload(ui.ctx()));
        });
        output.textures_delta.clear();
    }
}

#[test]
fn native_drag_does_not_drop_through_its_source_or_choose_an_ambiguous_window() {
    let source = ViewportId::from_hash_of("source");
    for overlapping_targets in [false, true] {
        let ctx = Context::default();
        ctx.add_plugin(NativeDrag::default());
        start(&ctx, 1.0);
        let point = if overlapping_targets {
            egui::pos2(550.0, 50.0)
        } else {
            egui::pos2(50.0, 50.0)
        };
        let mut raw = input(
            source,
            1.0,
            vec![Event::PointerButton {
                pos: point,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        if overlapping_targets {
            for name in ["target1", "target2"] {
                let mut target = raw.viewports[&ViewportId::ROOT].clone();
                target.parent = Some(ViewportId::ROOT);
                raw.viewports.insert(ViewportId::from_hash_of(name), target);
            }
        } else {
            raw.viewports.get_mut(&ViewportId::ROOT).unwrap().inner_rect =
                raw.viewports[&source].inner_rect;
        }
        let mut output = ctx.run_ui(raw.clone(), |_| {});
        output.textures_delta.clear();
        raw.viewport_id = ViewportId::ROOT;
        raw.events.clear();
        let mut output = ctx.run_ui(raw, |ui| {
            assert!(!ui.input(|i| i.pointer.any_released()));
            assert!(!egui::DragAndDrop::has_any_payload(ui.ctx()));
        });
        output.textures_delta.clear();
    }
}

#[test]
fn native_drag_cancels_when_the_source_closes() {
    let ctx = Context::default();
    ctx.add_plugin(NativeDrag::default());
    start(&ctx, 1.0);
    let mut raw = input(ViewportId::ROOT, 1.0, vec![]);
    raw.viewports.remove(&ViewportId::from_hash_of("source"));
    let mut output = ctx.run_ui(raw, |ui| {
        assert!(!egui::DragAndDrop::has_any_payload(ui.ctx()));
        assert!(!ui.input(|i| i.pointer.any_released()));
    });
    output.textures_delta.clear();
}
